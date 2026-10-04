//! The edit list (#923): the sequence's clips, transitions and markers in
//! one sortable, filterable table, with track, name, source and record
//! In/Out, and duration. Clicking a row selects what it names and puts
//! the playhead on it, so a shot is found in a long sequence by name.

use crate::state::AppState;
use klipp_core::{AppCommand, ClipId, MarkerId, RationalTime, TransitionId};
use klipp_media::MediaPool;
use klipp_timeline::{ClipKind, Sequence};

/// What a row names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Clip(ClipId),
    /// A transition, with the index of its track.
    Transition(usize, TransitionId),
    Marker(MarkerId),
}

/// One line of the edit list.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub target: Target,
    /// "Clip", "Title", "Transition", "Marker"…
    pub kind: &'static str,
    /// The track's name; empty for a marker, which is on the sequence.
    pub track: String,
    /// Its index, which orders tracks as the timeline does.
    pub track_index: usize,
    pub name: String,
    /// Where in its media it starts and ends; `None` for what has no
    /// media of its own.
    pub source: Option<(RationalTime, RationalTime)>,
    /// Where on the timeline it starts and ends.
    pub record: (RationalTime, RationalTime),
}

impl Row {
    pub fn duration(&self) -> RationalTime {
        self.record.1 - self.record.0
    }
}

/// The columns, as the table sorts by them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Column {
    Kind,
    Track,
    Name,
    SourceIn,
    #[default]
    RecordIn,
    Duration,
}

/// Every clip, transition and marker of `seq`, in timeline order.
pub fn rows(seq: &Sequence, pool: &MediaPool) -> Vec<Row> {
    let mut out = Vec::new();
    for (ti, track) in seq.tracks.iter().enumerate() {
        for clip in track.clips.iter().filter(|c| !c.kind.is_gap()) {
            let (kind, fallback, has_source) = match &clip.kind {
                ClipKind::Media { media_id } => ("Clip", pool.get(media_id).map(|m| m.filename.clone()).unwrap_or_else(|| "Missing media".into()), true),
                ClipKind::Title(td) => ("Title", td.text().lines().next().unwrap_or("").to_string(), false),
                ClipKind::Generator(_) => ("Generator", "Generator".to_string(), false),
                ClipKind::AdjustmentLayer => ("Adjustment", "Adjustment layer".to_string(), false),
                ClipKind::Sequence { .. } => ("Nested", "Nested sequence".to_string(), true),
                ClipKind::Precomposition { .. } | ClipKind::TemplatePrecomposition { .. } => ("Composition", "Composition".to_string(), true),
                ClipKind::Gap => continue,
            };
            out.push(Row {
                target: Target::Clip(clip.id),
                kind,
                track: track.name.clone(),
                track_index: ti,
                name: clip.label(&fallback).to_string(),
                source: has_source.then_some((clip.source_in, clip.source_out)),
                record: (clip.timeline_in, clip.timeline_out),
            });
        }
        for tr in &track.transitions {
            let Some(record) = klipp_timeline::crossfade::window(track, tr) else { continue };
            out.push(Row {
                target: Target::Transition(ti, tr.id),
                kind: "Transition",
                track: track.name.clone(),
                track_index: ti,
                name: klipp_core::transition_registry::spec_for_kind(tr.kind).display_name.to_string(),
                source: None,
                record,
            });
        }
    }
    for m in &seq.markers {
        let end = m.duration.map(|d| m.position + d).unwrap_or(m.position);
        out.push(Row {
            target: Target::Marker(m.id),
            kind: "Marker",
            track: String::new(),
            track_index: usize::MAX,
            name: if m.label.trim().is_empty() { m.kind.label().to_string() } else { m.label.clone() },
            source: None,
            record: (m.position, end),
        });
    }
    sort(&mut out, Column::RecordIn, true);
    out
}

/// Sort `rows` by `column`, ties broken by record In and then track, so
/// the order is stable as the editor clicks between columns.
pub fn sort(rows: &mut [Row], column: Column, ascending: bool) {
    let secs = |t: RationalTime| t.to_secs_f64();
    rows.sort_by(|a, b| {
        let by = match column {
            Column::Kind => a.kind.cmp(b.kind),
            Column::Track => a.track_index.cmp(&b.track_index),
            Column::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            Column::SourceIn => a.source.map(|s| secs(s.0)).partial_cmp(&b.source.map(|s| secs(s.0))).unwrap_or(std::cmp::Ordering::Equal),
            Column::RecordIn => std::cmp::Ordering::Equal,
            Column::Duration => secs(a.duration()).partial_cmp(&secs(b.duration())).unwrap_or(std::cmp::Ordering::Equal),
        };
        let by = if ascending { by } else { by.reverse() };
        by.then_with(|| {
            let start = secs(a.record.0).partial_cmp(&secs(b.record.0)).unwrap_or(std::cmp::Ordering::Equal);
            if column == Column::RecordIn && !ascending { start.reverse() } else { start }
        })
        .then_with(|| a.track_index.cmp(&b.track_index))
    });
}

/// Whether `row` matches the filter: every word of `query` in its name,
/// kind or track, case ignored. An empty query matches every row.
pub fn matches(row: &Row, query: &str) -> bool {
    let hay = format!("{} {} {}", row.name, row.kind, row.track).to_lowercase();
    query.split_whitespace().all(|word| hay.contains(&word.to_lowercase()))
}

/// What clicking a row asks the app to do: select what it names and put
/// the playhead at its start. A transition is selected on the state, as
/// the timeline does when one is clicked.
pub fn activate(row: &Row, state: &mut AppState) -> Vec<AppCommand> {
    match row.target {
        Target::Clip(id) => {
            state.selected_transition = None;
            vec![AppCommand::SelectClip(Some(id)), AppCommand::SetPlayhead(row.record.0), AppCommand::CenterTimelineOnClip(id)]
        }
        Target::Transition(track, id) => {
            // The transition alone is selected, as a click on it does on
            // the timeline; the clips are let go.
            state.selected_clips.clear();
            state.selected_transition = Some((track, id));
            vec![AppCommand::SelectClip(None), AppCommand::SetPlayhead(row.record.0)]
        }
        Target::Marker(id) => vec![AppCommand::JumpToMarker { marker_id: id }],
    }
}

#[derive(Default)]
pub struct EditListPanel {
    query: String,
    column: Column,
    descending: bool,
}

impl EditListPanel {
    pub fn show(&mut self, ctx: &egui::Context, state: &mut AppState) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        let mut open = state.show_edit_list;
        egui::Window::new("Edit List")
            .open(&mut open)
            .default_width(640.0)
            .default_height(360.0)
            .resizable(true)
            .show(ctx, |ui| {
                cmds = self.body(ui, state);
            });
        state.show_edit_list = open;
        cmds
    }

    /// The filter, the column headers and the rows.
    pub fn body(&mut self, ui: &mut egui::Ui, state: &mut AppState) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Filter by name, kind or track\u{2026}").desired_width(ui.available_width()));
        ui.add_space(4.0);

        let mut all = rows(state.sequence(), &state.media_pool);
        sort(&mut all, self.column, !self.descending);
        let shown: Vec<&Row> = all.iter().filter(|r| matches(r, &self.query)).collect();
        let tc = state.timecode_display();
        let selected = |r: &Row| match r.target {
            Target::Clip(id) => state.selected_clip == Some(id) || state.selected_clips.contains(&id),
            Target::Transition(t, id) => state.selected_transition == Some((t, id)),
            Target::Marker(_) => false,
        };
        let mut clicked: Option<Row> = None;

        let headers: [(&str, Option<Column>, f32); 8] = [
            ("Kind", Some(Column::Kind), 74.0),
            ("Track", Some(Column::Track), 70.0),
            ("Name", Some(Column::Name), 160.0),
            ("Source In", Some(Column::SourceIn), 92.0),
            ("Source Out", None, 92.0),
            ("Record In", Some(Column::RecordIn), 92.0),
            ("Record Out", None, 92.0),
            ("Duration", Some(Column::Duration), 92.0),
        ];
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("edit_list_grid").striped(true).min_col_width(40.0).show(ui, |ui| {
                for (label, column, width) in headers {
                    let text = match column {
                        Some(c) if c == self.column => format!("{label} {}", if self.descending { "\u{25BC}" } else { "\u{25B2}" }),
                        _ => label.to_string(),
                    };
                    let header = egui::RichText::new(text).strong().size(11.0);
                    match column {
                        Some(c) => {
                            if ui.add_sized([width, 16.0], egui::Button::new(header).frame(false)).clicked() {
                                if self.column == c {
                                    self.descending = !self.descending;
                                } else {
                                    self.column = c;
                                    self.descending = false;
                                }
                            }
                        }
                        None => {
                            ui.add_sized([width, 16.0], egui::Label::new(header));
                        }
                    }
                }
                ui.end_row();
                for row in &shown {
                    let is_selected = selected(row);
                    let mut cell = |ui: &mut egui::Ui, text: String| {
                        if ui.selectable_label(is_selected, egui::RichText::new(text).size(11.0)).clicked() {
                            clicked = Some((*row).clone());
                        }
                    };
                    cell(ui, row.kind.to_string());
                    cell(ui, row.track.clone());
                    cell(ui, row.name.clone());
                    cell(ui, row.source.map(|s| tc.position(s.0)).unwrap_or_default());
                    cell(ui, row.source.map(|s| tc.position(s.1)).unwrap_or_default());
                    cell(ui, tc.position(row.record.0));
                    cell(ui, tc.position(row.record.1));
                    cell(ui, tc.duration(row.duration()));
                    ui.end_row();
                }
            });
            if shown.is_empty() {
                ui.label(egui::RichText::new(if all.is_empty() { "The sequence is empty." } else { "Nothing matches the filter." }).size(11.0).weak());
            }
        });
        if let Some(row) = clicked {
            cmds.extend(activate(&row, state));
        }
        cmds
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::MediaId;
    use klipp_timeline::{Clip, Marker};

    fn tc(secs: i64) -> RationalTime {
        RationalTime { value: secs * 24, rate: 24 }
    }

    /// Two clips on the first video track, one named, with a dissolve
    /// between them, a clip on the first audio track and a marker.
    fn fixture() -> (Sequence, MediaPool, ClipId, ClipId) {
        let mut seq = Sequence::new_default();
        let mut pool = MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/beach.mov".into(), None);
        item.duration = tc(60);
        let media: MediaId = pool.add(item);
        let v = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        let a = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio).unwrap();
        let first = Clip::media(media, tc(0), tc(4), tc(10), tc(14));
        let mut second = Clip::media(media, tc(4), tc(10), tc(20), tc(26));
        second.name = Some("Hero shot".into());
        let (first_id, second_id) = (first.id, second.id);
        seq.tracks[v].clips = vec![first, second];
        seq.tracks[v].transitions.push(klipp_timeline::Transition {
            id: TransitionId::new(),
            kind: klipp_core::TransitionKind::CrossDissolve,
            duration: tc(2),
            clip_a_id: first_id,
            clip_b_id: second_id,
            user_params: Default::default(),
            alignment: klipp_core::TransitionAlignment::Center,
        });
        seq.tracks[a].clips.push(Clip::media(media, tc(1), tc(3), tc(0), tc(2)));
        seq.markers.push(Marker {
            id: MarkerId::new(),
            position: tc(7),
            label: "Laugh".into(),
            color: klipp_core::MarkerColor::Red,
            duration: Some(tc(1)),
            comment: String::new(),
            kind: Default::default(),
        });
        (seq, pool, first_id, second_id)
    }

    #[test]
    fn every_clip_transition_and_marker_is_listed_in_timeline_order() {
        let (seq, pool, _, second) = fixture();
        let rows = rows(&seq, &pool);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["beach.mov", "beach.mov", "Cross Dissolve", "Hero shot", "Laugh"]);
        let hero = rows.iter().find(|r| r.target == Target::Clip(second)).unwrap();
        assert_eq!(hero.source, Some((tc(20), tc(26))), "a clip's source In/Out");
        assert_eq!((hero.record, hero.duration()), ((tc(4), tc(10)), tc(6)));
        let dissolve = rows.iter().find(|r| r.kind == "Transition").unwrap();
        assert_eq!(dissolve.record, (tc(3), tc(5)), "centred on the cut");
        let laugh = rows.iter().find(|r| r.kind == "Marker").unwrap();
        assert_eq!((laugh.record, laugh.source), ((tc(7), tc(8)), None));
    }

    #[test]
    fn rows_sort_by_any_column_either_way() {
        let (seq, pool, _, _) = fixture();
        let mut rows = rows(&seq, &pool);
        sort(&mut rows, Column::Duration, false);
        assert_eq!(rows[0].name, "Hero shot", "the longest first");
        sort(&mut rows, Column::Name, true);
        assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["beach.mov", "beach.mov", "Cross Dissolve", "Hero shot", "Laugh"]);
        sort(&mut rows, Column::RecordIn, false);
        assert_eq!(rows[0].name, "Laugh", "latest first");
        sort(&mut rows, Column::Track, true);
        assert_eq!(rows.last().unwrap().kind, "Marker", "markers, on no track, sort after the tracks");
    }

    #[test]
    fn the_filter_needs_every_word() {
        let (seq, pool, _, _) = fixture();
        let rows = rows(&seq, &pool);
        let hits = |q: &str| rows.iter().filter(|r| matches(r, q)).count();
        assert_eq!(hits(""), rows.len());
        assert_eq!(hits("hero"), 1);
        assert_eq!(hits("BEACH"), 2);
        assert_eq!(hits("transition"), 1);
        assert_eq!(hits("hero dissolve"), 0);
    }

    /// The panel drawn with `events`: the text drawn, with where, and
    /// what it asked for.
    fn draw(panel: &mut EditListPanel, state: &mut AppState, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) -> (Vec<(String, egui::Pos2)>, Vec<AppCommand>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 500.0))),
            time: Some(time),
            events,
            ..Default::default()
        };
        let mut cmds = Vec::new();
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                cmds = panel.body(ui, state);
            });
        });
        let drawn = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
            _ => None,
        }).collect();
        (drawn, cmds)
    }

    #[test]
    fn the_drawn_list_sorts_by_a_header_and_a_row_click_selects() {
        let (seq, pool, _, second) = fixture();
        let mut state = AppState { media_pool: pool, ..Default::default() };
        state.sequences[0] = seq;
        let (ctx, mut panel) = (egui::Context::default(), EditListPanel::default());
        let mut time = 0.0;
        let mut frame = |panel: &mut EditListPanel, state: &mut AppState, events| { time += 0.5; draw(panel, state, &ctx, time, events) };
        let click = |at: egui::Pos2| -> Vec<Vec<egui::Event>> {
            let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            vec![vec![egui::Event::PointerMoved(at), button(true)], vec![button(false)]]
        };
        let (drawn, _) = frame(&mut panel, &mut state, Vec::new());
        assert!(drawn.iter().any(|(t, _)| t.starts_with("Record In")), "the headers are drawn: {drawn:?}");
        let duration = drawn.iter().find(|(t, _)| t == "Duration").unwrap().1;
        for events in click(duration) {
            frame(&mut panel, &mut state, events);
        }
        let (drawn, _) = frame(&mut panel, &mut state, Vec::new());
        let y = |name: &str| drawn.iter().find(|(t, _)| t == name).unwrap().1.y;
        assert!(y("Laugh") < y("Hero shot"), "sorted by duration, shortest first: the one-second marker above the six-second hero");
        let hero = drawn.iter().find(|(t, _)| t == "Hero shot").unwrap().1;
        let mut sent = Vec::new();
        for events in click(hero) {
            sent.extend(frame(&mut panel, &mut state, events).1);
        }
        assert!(sent.iter().any(|c| matches!(c, AppCommand::SelectClip(Some(id)) if *id == second)), "{sent:?}");
    }

    #[test]
    fn clicking_a_row_selects_it_and_moves_the_playhead() {
        let (seq, pool, first, _) = fixture();
        let mut state = AppState::default();
        state.sequences[0] = seq.clone();
        let rows = rows(&seq, &pool);
        let clip = rows.iter().find(|r| r.target == Target::Clip(first)).unwrap();
        let cmds = activate(clip, &mut state);
        assert!(matches!(cmds.as_slice(), [AppCommand::SelectClip(Some(id)), AppCommand::SetPlayhead(t), AppCommand::CenterTimelineOnClip(_)] if *id == first && *t == tc(0)), "{cmds:?}");
        let dissolve = rows.iter().find(|r| r.kind == "Transition").unwrap();
        let cmds = activate(dissolve, &mut state);
        assert!(matches!(state.selected_transition, Some((_, _))), "the transition is selected");
        assert!(matches!(cmds.as_slice(), [AppCommand::SelectClip(None), AppCommand::SetPlayhead(t)] if *t == tc(3)), "the clips let go: {cmds:?}");
        let marker = rows.iter().find(|r| r.kind == "Marker").unwrap();
        assert!(matches!(activate(marker, &mut state).as_slice(), [AppCommand::JumpToMarker { .. }]));
    }
}
