//! The selects edited as a timeline of their own, stacked above the cut
//! (#82). Two timelines share one app state: the one being worked in is
//! the state's own, as a single timeline is, so every edit, key and
//! shortcut goes to it unchanged; the other's sequence, playhead, zoom,
//! selection and drags are parked in a [`TimelineView`]. The parked one
//! is drawn by trading its view in and back out, and what it would do is
//! dropped, until it is pressed: then the two trade places through the
//! sequence tabs' own switch, and the press goes to it.

use super::NewTimelinePanel;
use crate::state::{AppState, TimelineView};
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

/// The stacked header's height.
pub const HEADER: f32 = 22.0;

/// The upper timeline: its own panels, so its drags and caches are its
/// own; the view of whichever of the two is parked; and which sequence it
/// shows.
pub struct Stacked {
    panel: NewTimelinePanel,
    old: crate::panels::timeline::TimelinePanel,
    pub view: TimelineView,
    pub top_idx: usize,
}

impl Stacked {
    /// The selects at `top_idx` stacked over what the state shows: the
    /// one not being worked in parked, from its sequence's own playhead.
    fn new(state: &AppState, top_idx: usize) -> Self {
        let parked = if state.active_sequence_idx == top_idx { (0..state.sequences.len()).find(|i| *i != top_idx).unwrap_or(top_idx) } else { top_idx };
        let rate = state.sequences[parked].fps.rate();
        let playhead = state.sequences[parked].last_playhead.unwrap_or(klipp_core::RationalTime::from_frames(0, rate));
        let mut old = crate::panels::timeline::TimelinePanel::default();
        old.embedded = true;
        Stacked { panel: NewTimelinePanel::default(), old, view: TimelineView::fresh(parked, playhead, state.timeline_zoom), top_idx }
    }

    /// Whether this still fits the state: the selects at `top_idx`, and
    /// the parked view the other of the two.
    fn fits(&self, state: &AppState, top_idx: usize) -> bool {
        let parked = self.view.active_sequence_idx;
        self.top_idx == top_idx && parked < state.sequences.len() && parked != state.active_sequence_idx && (parked == top_idx || state.active_sequence_idx == top_idx)
    }
}

/// Whether the parked timeline in `rect` is taken this frame: pressed, or
/// a range carried down from the selects let go over it.
pub(super) fn taken(ui: &egui::Ui, state: &AppState, rect: egui::Rect) -> bool {
    ui.input(|i| {
        (i.pointer.any_pressed() && i.pointer.press_origin().is_some_and(|p| rect.contains(p)))
            || (state.selects_drag.is_some() && i.pointer.any_released() && i.pointer.latest_pos().is_some_and(|p| rect.contains(p)))
    })
}

/// The parked timeline drawn by `draw` with its view traded in. Unless it
/// is `take`n, what it does is dropped, so nothing it would send reaches
/// the engine or the sequence being worked in. Taken, it becomes the one
/// worked in ([`focus`]) and what it did follows. Whether it was taken.
pub(super) fn draw_parked(view: &mut TimelineView, state: &mut AppState, take: bool, cmds: &mut Vec<AppCommand>, draw: impl FnOnce(&mut AppState, &mut Vec<AppCommand>)) -> bool {
    view.swap(state);
    let mut drawn = Vec::new();
    draw(state, &mut drawn);
    view.swap(state);
    if take {
        focus(view, state, cmds);
        cmds.extend(drawn);
    }
    take
}

/// The parked timeline made the one worked in: the two views traded, and
/// the sequence switched as a sequence tab switches it (its playhead kept
/// on its sequence and the other's taken back, playback stopped, the
/// engine given the sequence), the switch queued ahead of anything else.
fn focus(view: &mut TimelineView, state: &mut AppState, cmds: &mut Vec<AppCommand>) {
    let to = view.active_sequence_idx;
    state.sequences[to].last_playhead = Some(view.playhead);
    let (from, playhead, smooth, anchor) = (state.active_sequence_idx, state.playhead, state.playhead_smooth_secs, state.playhead_anchor);
    view.swap(state);
    // Until the switch runs the state still shows the one left, so the
    // switch saves its playhead and takes the other's from its sequence.
    state.active_sequence_idx = from;
    state.playhead = playhead;
    state.playhead_smooth_secs = smooth;
    state.playhead_anchor = anchor;
    cmds.insert(0, AppCommand::SetActiveSequence { sequence_idx: to });
}

/// The upper timeline in `rect`, its header first: drawn from the state
/// when it is the one worked in, else parked. A range carried down from
/// its header goes to the cut in `cut`. Whether the selects became the
/// one worked in this frame.
#[allow(clippy::too_many_arguments)]
pub(super) fn show_top(main: &mut NewTimelinePanel, ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme, rect: egui::Rect, cut: egui::Rect, cmds: &mut Vec<AppCommand>) -> bool {
    let Some(top_idx) = state.selects_sequence.and_then(|id| state.sequences.iter().position(|s| s.id == id)) else { return false };
    let mut st = match main.stacked.take() {
        Some(st) if st.fits(state, top_idx) => st,
        _ => Box::new(Stacked::new(state, top_idx)),
    };
    let top_live = state.active_sequence_idx == top_idx;
    let header = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), HEADER));
    let lanes = egui::Rect::from_min_max(egui::pos2(rect.min.x, header.max.y), rect.max);
    header_row(ui, state, theme, header, top_idx, if top_live { None } else { Some(&st.view) }, cut);
    let changed = if top_live {
        st.panel.show_content(ui, state, theme, &mut st.old, false, lanes, cmds);
        false
    } else {
        let take = taken(ui, state, rect);
        let Stacked { panel, old, view, .. } = &mut *st;
        draw_parked(view, state, take, cmds, |state, out| panel.show_content(ui, state, theme, old, false, lanes, out))
    };
    // The one worked in, outlined.
    let worked = if top_live != changed { rect } else { cut };
    ui.painter().rect_stroke(worked.shrink(0.5), 0.0, egui::Stroke::new(1.0_f32, theme.accent.gamma_multiply(0.6)));
    main.stacked = Some(st);
    changed
}

/// The header: what the upper timeline is, the way back to the strip, and
/// the handle that carries its marked range (its In to Out, else its
/// selected clips' span) down onto the cut.
fn header_row(ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme, rect: egui::Rect, top_idx: usize, parked: Option<&TimelineView>, cut: egui::Rect) {
    let seq = &state.sequences[top_idx];
    let (id, name) = (seq.id, seq.name.clone());
    let marked = match (seq.in_point, seq.out_point) {
        (Some(a), Some(b)) if b > a => Some((a, b)),
        _ => {
            let selected = parked.map_or(&state.selected_clips, |v| &v.selected_clips);
            let spans: Vec<_> = seq.all_clips().filter(|c| selected.contains(&c.id)).map(|c| (c.timeline_in, c.timeline_out)).collect();
            let a = spans.iter().map(|s| s.0).min();
            let b = spans.iter().map(|s| s.1).max();
            a.zip(b).filter(|(a, b)| b > a)
        }
    };
    ui.painter().rect_filled(rect, 0.0, theme.bg_raised);
    ui.painter().hline(rect.x_range(), rect.max.y - 0.5, egui::Stroke::new(1.0_f32, theme.border));
    let mut carry = false;
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(8.0, 1.0))), |ui| {
        ui.horizontal_centered(|ui| {
            ui.label(egui::RichText::new("SELECTS").size(10.0).strong().color(theme.accent));
            ui.label(egui::RichText::new(&name).size(11.0).color(theme.text_secondary));
            if ui.small_button("Done").on_hover_text("Back to the read-only strip").clicked() {
                state.selects_edit = false;
            }
            ui.separator();
            let label = match marked {
                Some(_) => "\u{21E9} Drag into the cut",
                None => "Mark In and Out, or select clips, to drag them into the cut",
            };
            let handle = ui.add_enabled(marked.is_some(), egui::Label::new(egui::RichText::new(label).size(11.0).color(if marked.is_some() { theme.text_primary } else { theme.text_dim })).sense(egui::Sense::drag()));
            carry = handle.on_hover_cursor(egui::CursorIcon::Grab).drag_started();
        });
    });
    if let (true, Some((a, b))) = (carry, marked) {
        state.selects_drag = Some((id, a, b));
    }
    // Let go anywhere but over the cut: carried nowhere.
    if state.selects_drag.is_some() && ui.input(|i| i.pointer.any_released() && !i.pointer.latest_pos().is_some_and(|p| cut.contains(p))) {
        state.selects_drag = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{MediaId, RationalTime, Timecode};
    use klipp_timeline::{Clip, TrackKind};

    fn tc(s: f64) -> Timecode {
        Timecode::from_secs_f64(s, 24)
    }

    /// The cut (sequence 0, a clip at 0..20 s) worked in, the selects
    /// (sequence 1, a clip at 0..4 s) stacked above it, edited.
    struct Stack {
        ctx: egui::Context,
        state: AppState,
        panel: NewTimelinePanel,
        old: crate::panels::timeline::TimelinePanel,
        time: f64,
        sent: Vec<AppCommand>,
    }

    impl Stack {
        fn new() -> Self {
            let mut state = AppState::default();
            let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/stack.mov".into(), None);
            item.duration = tc(600.0);
            let media: MediaId = state.media_pool.add(item);
            let v = state.sequences[0].tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
            state.sequences[0].tracks[v].clips.push(Clip::media(media, tc(0.0), tc(20.0), tc(0.0), tc(20.0)));
            let mut selects = klipp_timeline::Sequence::new_default();
            selects.name = "Selects".into();
            let sv = selects.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
            selects.tracks[sv].clips.push(Clip::media(media, tc(0.0), tc(4.0), tc(30.0), tc(34.0)));
            state.selects_sequence = Some(selects.id);
            state.sequences.push(selects);
            state.selects_edit = true;
            state.selects_height = 220.0;
            state.timeline_zoom = 40.0;
            let mut old = crate::panels::timeline::TimelinePanel::default();
            old.embedded = true;
            let mut s = Stack { ctx: egui::Context::default(), state, panel: NewTimelinePanel::default(), old, time: 0.0, sent: Vec::new() };
            s.frame(Vec::new());
            s
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
            self.time += 0.5;
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 700.0))), time: Some(self.time), events, ..Default::default() };
            let (theme, kb) = (KlippTheme::default(), crate::keybindings::KeyBindings::default());
            let (state, panel, old) = (&mut self.state, &mut self.panel, &mut self.old);
            let mut cmds = Vec::new();
            let out = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                    cmds = panel.show(ui, state, &theme, old, &kb, false);
                });
            });
            // The sequence switch as the app's sequence tabs make it.
            for cmd in &cmds {
                if let AppCommand::SetActiveSequence { sequence_idx } = cmd {
                    let from = self.state.active_sequence_idx;
                    self.state.sequences[from].last_playhead = Some(self.state.playhead);
                    self.state.active_sequence_idx = *sequence_idx;
                    let rate = self.state.sequences[*sequence_idx].fps.rate();
                    self.state.playhead = self.state.sequences[*sequence_idx].last_playhead.unwrap_or(RationalTime::from_frames(0, rate));
                    self.state.selected_clip = None;
                    self.state.selected_clips.clear();
                }
            }
            self.sent = cmds;
            out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)), _ => None }).collect()
        }

        fn view(&self) -> &TimelineView {
            &self.panel.stacked.as_ref().expect("stacked").view
        }

        fn press(&mut self, at: egui::Pos2) {
            let b = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            self.frame(vec![egui::Event::PointerMoved(at)]);
            self.frame(vec![b(true)]);
        }
    }

    /// Every field of the app's state that one timeline holds of its own
    /// is in the parked view, so none is left shared between the two;
    /// the others that sound per-timeline are shared on purpose.
    #[test]
    fn every_per_timeline_field_is_parked() {
        let source = include_str!("../state.rs");
        let body = &source[source.find("pub struct AppState {").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        let shared = [
            // The app's, not a timeline's: settings, popups, other panels.
            "playhead_follow", "playhead_follow_chosen", "playhead_anchor_wall", "playhead_speed", "selected_clip_loc_cache", "snap", "snap_threshold_px", "grid_snap_interval", "zoom_step",
            "skimming_enabled", "audio_skimming_enabled", "skim_speed_px_per_sec", "skim_last_update", "skim_last_x", "skim_audio_session", "skim_last_audio_dispatch", "program_skim_active", "program_skim_revert_deadline",
            "keyframe_context_menu", "automation_lane_param_menu", "keyframe_clipboard", "curve_key_clipboard", "selects_range", "selects_drag", "selects_edit", "selects_height", "selects_sequence",
            "source_playhead", "dragging_media", "dragging_media_group", "selected_media_ids", "browser_range_selection", "browser_drag_start", "selected_title_object", "title_object_drag",
            "viewer_transform_drag", "motion_path_drag", "viewer_corner_pin_drag", "viewer_mask_drag", "color_grade_drag_start", "resumable_path_drag", "selected_node_id", "selected_node_ids", "selected_node_addr",
            "selected_subtitle_track", "command_palette_selected", "title_find_selected", "workspace_manager_selected", "preset_manager_selected", "show_camera_lane", "file_browser_video_skim_enabled", "file_browser_audio_skim_enabled",
            "save_hyperzoom_preset_name", "save_hyperzoom_preset_error", "save_hyperzoom_preset_source_clip", "hyperzoom_preset_registry", "render_cache_ranges", "render_range", "scope_legal_range",
            "multicam_at_playhead", "waveform_zoom", "viewer_text_edit_select_all", "viewer_text_selection", "viewer_text_selection_locked", "viewer_text_select_anchor",
            "filmstrip_hover_enabled", "transcript_panel_hovered", "speed_dialog_ripple", "ripple_enabled", "linked_selection", "drag_drop", "surface_snaps_pending", "consolidate_include_skims", "project_auto_skim",
        ];
        let per_timeline = ["drag", "select", "hover", "lane", "zoom", "scroll", "playhead", "range", "trim", "key_selection", "keyframe"];
        let mut missing = Vec::new();
        for line in body.lines() {
            let Some(name) = line.strip_prefix("    pub ").and_then(|l| l.split(':').next()) else { continue };
            let looks = per_timeline.iter().any(|w| name.contains(w)) && !name.starts_with("show_") && !name.contains("skim_") && !name.contains("browser_skim");
            if looks && !TimelineView::FIELDS.contains(&name) && !shared.contains(&name) {
                missing.push(name.to_string());
            }
        }
        assert!(missing.is_empty(), "per-timeline fields neither parked nor named shared: {missing:?}");
    }

    /// Drawn without a press, the parked selects keep their own zoom and
    /// selection and the cut's are untouched; a press on them makes them
    /// the one worked in through the tabs' switch, and the cut is parked
    /// with what it had.
    #[test]
    fn a_press_trades_the_two_and_each_keeps_its_own() {
        let mut s = Stack::new();
        let cut_clip = s.state.sequences[0].all_clips().next().unwrap().id;
        s.state.selected_clip = Some(cut_clip);
        s.panel.stacked.as_mut().unwrap().view.timeline_zoom = 120.0;
        for _ in 0..3 {
            s.frame(vec![egui::Event::PointerMoved(egui::pos2(500.0, 120.0))]);
        }
        assert_eq!((s.state.active_sequence_idx, s.state.timeline_zoom, s.state.selected_clip), (0, 40.0, Some(cut_clip)), "the cut as it was");
        assert_eq!((s.view().active_sequence_idx, s.view().timeline_zoom), (1, 120.0), "the selects parked as they were");
        assert!(!s.sent.iter().any(|c| matches!(c, AppCommand::SetActiveSequence { .. })));
        s.press(egui::pos2(600.0, 150.0));
        assert!(matches!(s.sent.first(), Some(AppCommand::SetActiveSequence { sequence_idx: 1 })), "{:?}", s.sent);
        assert_eq!((s.state.active_sequence_idx, s.state.timeline_zoom), (1, 120.0), "the selects worked in, at their zoom");
        assert_eq!((s.view().active_sequence_idx, s.view().timeline_zoom), (0, 40.0), "the cut parked at its zoom");
    }

    /// Both timelines draw their track headers, the real ones.
    #[test]
    fn both_timelines_draw_their_track_headers() {
        let mut s = Stack::new();
        let texts = s.frame(Vec::new());
        let video = s.state.sequences[0].tracks.iter().find(|t| t.kind == TrackKind::Video).unwrap().name.clone();
        let ys: Vec<f32> = texts.iter().filter(|(t, _)| *t == video).map(|(_, p)| p.y).collect();
        assert!(ys.len() >= 2 && ys.iter().any(|y| *y < 260.0) && ys.iter().any(|y| *y > 300.0), "{video:?} in both: {ys:?}");
    }

    /// The selects' marked range carried from their header and let go
    /// over the cut goes into the cut where it is let go, the cut worked
    /// in again first; the copy is one edit undo takes back.
    #[test]
    fn a_range_carried_down_goes_into_the_cut() {
        let mut s = Stack::new();
        s.state.sequences[1].in_point = Some(tc(0.0));
        s.state.sequences[1].out_point = Some(tc(4.0));
        let texts = s.frame(Vec::new());
        let handle = texts.iter().find(|(t, _)| t.contains("Drag into the cut")).expect("the handle").1;
        let to = egui::pos2(500.0, 345.0);
        let b = |p, d| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: d, modifiers: egui::Modifiers::NONE };
        s.frame(vec![egui::Event::PointerMoved(handle), b(handle, true)]);
        let mut sent = s.sent.clone();
        for i in 1..=6 {
            s.frame(vec![egui::Event::PointerMoved(handle + (to - handle) * (i as f32 / 6.0))]);
            sent.extend(s.sent.clone());
        }
        assert!(s.state.selects_drag.is_some(), "carried");
        s.frame(vec![egui::Event::PointerMoved(to), b(to, false)]);
        sent.extend(s.sent.clone());
        let id = s.state.sequences[1].id;
        let edit = sent.iter().position(|c| matches!(c, AppCommand::EditFromSelects { sequence_id, range_in, range_out, at: Some(_), overwrite: false } if *sequence_id == id && *range_in == tc(0.0) && *range_out == tc(4.0)));
        assert!(edit.is_some(), "{sent:?}");
        assert_eq!(s.state.active_sequence_idx, 0, "the cut worked in when it takes it");

        // The copy itself, as the app makes it from that command: one set
        // of track edits, which undone leave the cut as it was.
        let before = s.state.sequences[0].clone();
        let cb = crate::clip_clipboard::copy_range(&s.state.sequences[1], tc(0.0), tc(4.0)).expect("the range");
        let pasted = crate::clip_clipboard::paste(&before, &cb, tc(5.0), crate::clip_clipboard::PasteMode::Insert);
        assert_eq!(pasted.ids.len(), 1);
        let edits = crate::clip_clipboard::track_edits(&before, &pasted.sequence, &pasted.touched, "Insert from Selects");
        let mut after = vec![pasted.sequence.clone()];
        let mut view = klipp_document::apply::DocumentView { sequences: &mut after, active: 0, media_pool: &mut s.state.media_pool, fps: &mut s.state.fps };
        for e in edits.iter().rev() {
            view.apply_inverse(e);
        }
        assert_eq!(after[0].all_clips().count(), before.all_clips().count(), "undone");
    }
}
