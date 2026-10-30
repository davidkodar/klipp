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

/// The height of the accent band across the top of the timeline worked
/// in (#2324).
pub const FOCUS_BAR: f32 = 3.0;

/// Mark `worked`, the one of the two timelines that takes edits: an accent
/// band across its top and a thin outline (#2324). Its left edge is the
/// tracks' colour stripes, so the mark is along the top. Painted after
/// both are drawn, so neither covers it.
pub fn paint_focus(ui: &egui::Ui, theme: &KlippTheme, worked: egui::Rect, other_ruler: egui::Rect) {
    let painter = ui.painter();
    painter.rect_stroke(worked.shrink(0.5), 0.0, egui::Stroke::new(1.0_f32, theme.accent.gamma_multiply(0.8)));
    painter.rect_filled(egui::Rect::from_min_size(worked.min, egui::vec2(worked.width(), FOCUS_BAR)), 0.0, theme.accent);
    // The other steps back: a veil over its ruler, its playhead's head
    // dimmed with it; its clips stay as they are, to be read and picked.
    painter.rect_filled(other_ruler, 0.0, theme.bg_deep.gamma_multiply(0.55));
}

/// The height the upper timeline needs to show every track of sequence
/// `idx` (#2324): its header, its ruler and its tracks, never above
/// [`MAX_HEIGHT`].
pub fn fitting_height(state: &AppState, idx: usize) -> f32 {
    let tracks: f32 = state.sequences.get(idx).map_or(0.0, |s| s.tracks.iter().map(crate::panels::timeline::track_row_height).sum());
    (HEADER + crate::theme::RULER_HEIGHT + tracks + 8.0).min(MAX_HEIGHT)
}

/// The tallest the stacked selects may be dragged.
pub const MAX_HEIGHT: f32 = 400.0;

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
        Stacked { panel: NewTimelinePanel::default(), old, view: TimelineView::fresh(parked, state.sequences[parked].fps, playhead, state.timeline_zoom), top_idx }
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
/// engine given the sequence) but keeping the selection the view brought,
/// the switch queued ahead of anything else.
pub(crate) fn focus(view: &mut TimelineView, state: &mut AppState, cmds: &mut Vec<AppCommand>) {
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
    cmds.insert(0, AppCommand::FocusSequence { sequence_idx: to });
}

/// The upper timeline in `rect`, its header first: drawn from the state
/// when it is the one worked in, else parked. A range carried down from
/// its header goes to the cut in `cut`. Whether the selects became the
/// one worked in this frame.
#[allow(clippy::too_many_arguments)]
pub(super) fn show_top(main: &mut NewTimelinePanel, ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme, rect: egui::Rect, cut: egui::Rect, kb: &crate::keybindings::KeyBindings, cmds: &mut Vec<AppCommand>) -> bool {
    let Some(top_idx) = state.selects_sequence.and_then(|id| state.sequences.iter().position(|s| s.id == id)) else { return false };
    let mut st = match main.stacked.take() {
        Some(st) if st.fits(state, top_idx) => st,
        _ => Box::new(Stacked::new(state, top_idx)),
    };
    let top_live = state.active_sequence_idx == top_idx;
    let header = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), HEADER));
    let lanes = egui::Rect::from_min_max(egui::pos2(rect.min.x, header.max.y), rect.max);
    let done = header_row(ui, state, theme, header, top_idx, if top_live { None } else { Some(&st.view) }, cut);
    if done {
        // Back to the strip: the cut worked in again first, so the main
        // timeline is left on the cut, not the selects.
        if top_live {
            focus(&mut st.view, state, cmds);
        }
        state.selects_edit = false;
        return top_live;
    }
    // Under its own id, so its widgets are not the cut's.
    let changed = ui.push_id("stacked_selects", |ui| {
        if top_live {
            st.panel.show_content(ui, state, theme, &mut st.old, false, lanes, kb, cmds);
            false
        } else {
            let take = taken(ui, state, rect);
            let Stacked { panel, old, view, .. } = &mut *st;
            draw_parked(view, state, take, cmds, |state, out| panel.show_content(ui, state, theme, old, false, lanes, kb, out))
        }
    }).inner;
    main.stacked = Some(st);
    changed
}

/// The header: what the upper timeline is, the way back to the strip, and
/// the handle that carries its marked range (its In to Out, else its
/// selected clips' span) down onto the cut.
/// Whether Done was pressed.
fn header_row(ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme, rect: egui::Rect, top_idx: usize, parked: Option<&TimelineView>, cut: egui::Rect) -> bool {
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
    let mut done = false;
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(8.0, 1.0))), |ui| {
        ui.horizontal_centered(|ui| {
            ui.label(egui::RichText::new("SELECTS").size(10.0).strong().color(theme.accent));
            ui.label(egui::RichText::new(&name).size(11.0).color(theme.text_secondary));
            done = ui.small_button("Done").on_hover_text("Back to the read-only strip").clicked();
            ui.separator();
            // The handle once something is marked; until then a hint at
            // readable contrast (a disabled label greys it past reading).
            if marked.is_some() {
                let handle = ui.add(egui::Label::new(egui::RichText::new("\u{21E9} Drag into the cut").size(11.0).color(theme.text_primary)).sense(egui::Sense::drag()));
                carry = handle.on_hover_cursor(egui::CursorIcon::Grab).drag_started();
            } else {
                ui.label(egui::RichText::new("Mark In and Out, or select clips, to drag them into the cut").size(11.0).color(theme.text_secondary));
            }
        });
    });
    if let (true, Some((a, b))) = (carry, marked) {
        state.selects_drag = Some((id, a, b));
    }
    // Let go anywhere but over the cut: carried nowhere.
    if state.selects_drag.is_some() && ui.input(|i| i.pointer.any_released() && !i.pointer.latest_pos().is_some_and(|p| cut.contains(p))) {
        state.selects_drag = None;
    }
    done
}

/// A Ctrl-drag begun on clip `hit` with the selects stacked and the
/// Select tool armed: it, or the selection it is in, carried as a copy to
/// the other timeline, not moved. Whether it began one (a plain drag,
/// another tool, or nothing stacked, does not).
pub fn start_carry(state: &mut AppState, hit: klipp_core::ClipId, ctrl: bool) -> bool {
    // Only the Select tool's drag moves clips (the Text and Mask tools'
    // too, as the timeline treats them as Select); another tool's
    // Ctrl-drag stays that tool's (#2318).
    if !ctrl || !stacked_edit(state) || !matches!(state.active_tool, klipp_core::Tool::Select | klipp_core::Tool::Text | klipp_core::Tool::Mask) {
        return false;
    }
    let mut ids = state.selected_clips.clone();
    if let Some(primary) = state.selected_clip.filter(|p| !ids.contains(p)) {
        ids.push(primary);
    }
    if !ids.contains(&hit) {
        ids = vec![hit];
    }
    state.clip_carry = Some((state.sequence().id, ids));
    true
}

/// Whether the selects are stacked above the cut as a timeline of their
/// own: switched on, still a sequence, and not on the Colour page's strip.
pub fn stacked_edit(state: &AppState) -> bool {
    !state.show_color_panel && state.selects_edit && state.selects_sequence.is_some_and(|id| state.sequences.iter().any(|s| s.id == id))
}

/// How near, in points, a carried copy snaps to the playhead or a clip's
/// edge, as a clip's drag does.
const SNAP_PX: f32 = 8.0;

/// The clips being carried, each frame: over the other timeline, a ghost
/// of them where they would land (snapped to its playhead or a clip's
/// edge, else to a frame), on its track under the pointer; let go there,
/// that timeline worked in and the copy made in one step (an overwrite
/// with Alt). Let go over their own timeline or nowhere, nothing.
#[allow(clippy::too_many_arguments)]
pub(super) fn carry_frame(st: &mut Stacked, ui: &egui::Ui, state: &mut AppState, theme: &KlippTheme, top: egui::Rect, cut: egui::Rect, cut_tracks: &[(usize, klipp_timeline::TrackKind, egui::Rect)], cmds: &mut Vec<AppCommand>) {
    let Some((from, ids)) = state.clip_carry.clone() else { return };
    let top_id = state.sequences[st.top_idx].id;
    let cut_idx = if state.active_sequence_idx == st.top_idx { st.view.active_sequence_idx } else { state.active_sequence_idx };
    let (rect, target) = if from == top_id { (cut, cut_idx) } else { (egui::Rect::from_min_max(egui::pos2(top.min.x, top.min.y + HEADER), top.max), st.top_idx) };
    let live = state.active_sequence_idx == target;
    let (zoom, scroll, playhead) = if live { (state.timeline_zoom, state.timeline_scroll_x, state.playhead) } else { (st.view.timeline_zoom, st.view.timeline_scroll_x, st.view.playhead) };
    let zoom = zoom.max(1e-3);
    let fps = state.sequences[target].fps;
    let left = rect.min.x + state.track_header_width;
    let x_of = |secs: f64| left + secs as f32 * zoom - scroll;
    let over = ui.input(|i| i.pointer.latest_pos()).filter(|p| rect.contains(*p) && p.x >= left);
    // The target's track under the pointer, as it was drawn this frame.
    let tracks = if from == top_id { cut_tracks.to_vec() } else { st.old.frame_cache.track_rects.clone() };
    let lane = over.and_then(|p| tracks.iter().find(|(_, _, r)| r.y_range().contains(p.y)).map(|(i, _, r)| (*i, *r)));
    let Some(source) = state.sequences.iter().find(|s| s.id == from) else {
        state.clip_carry = None;
        return;
    };
    let carried: Vec<&klipp_timeline::Clip> = source.all_clips().filter(|c| ids.contains(&c.id)).collect();
    let span = carried.iter().map(|c| c.timeline_out).max().zip(carried.iter().map(|c| c.timeline_in).min()).map_or(0.0, |(b, a)| (b - a).to_secs_f64());
    let at = over.map(|p| {
        let secs = (((p.x - left + scroll) / zoom) as f64).max(0.0);
        let edges = state.sequences[target].all_clips().flat_map(|c| [c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64()]);
        let near = std::iter::once(playhead.to_secs_f64()).chain(edges).map(|e| (e, (x_of(e) - p.x).abs())).filter(|(_, d)| *d <= SNAP_PX).min_by(|a, b| a.1.total_cmp(&b.1));
        match near {
            Some((e, _)) => e,
            None => (secs * fps.as_f64()).round() / fps.as_f64(),
        }
    });
    let overwrite = ui.input(|i| i.modifiers.alt);
    if let (Some(p), Some(at)) = (over, at) {
        let ghost = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, ui.id().with("clip_carry_ghost")));
        // On the track it goes to, else under the pointer.
        let (y0, y1) = lane.map_or((p.y - 12.0, p.y + 12.0), |(_, r)| (r.min.y + 2.0, r.max.y - 2.0));
        let shape = egui::Rect::from_min_max(egui::pos2(x_of(at), y0), egui::pos2(x_of(at + span).max(x_of(at) + 4.0), y1));
        ghost.rect_filled(shape, 3.0, theme.accent.gamma_multiply(0.25));
        ghost.rect_stroke(shape, 3.0, egui::Stroke::new(1.0_f32, theme.accent));
        let n = carried.len();
        let what = if overwrite { "Overwrite" } else { "Insert" };
        let text = format!("{what} {n} clip{} at {}", if n == 1 { "" } else { "s" }, state.timecode_display().position_secs(at));
        ghost.text(egui::pos2(shape.min.x + 4.0, shape.max.y + 2.0), egui::Align2::LEFT_TOP, text, egui::FontId::proportional(11.0), theme.text_primary);
        ui.ctx().request_repaint();
    }
    if ui.input(|i| i.pointer.any_released()) {
        state.clip_carry = None;
        if let Some(at) = at {
            if !live {
                focus(&mut st.view, state, cmds);
            }
            cmds.push(AppCommand::PasteClipsFrom { from, ids, at: klipp_core::Timecode::from_secs_f64(at, fps.rate()), overwrite, track: lane.map(|(i, _)| i), linked: state.linked_selection });
        }
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
        /// The modifiers held through each frame.
        mods: egui::Modifiers,
        /// What the last frame drew.
        shapes: Vec<egui::epaint::ClippedShape>,
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
            let mut s = Stack { ctx: egui::Context::default(), state, panel: NewTimelinePanel::default(), old, time: 0.0, sent: Vec::new(), mods: egui::Modifiers::NONE, shapes: Vec::new() };
            s.frame(Vec::new());
            s
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
            self.time += 0.5;
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 700.0))), time: Some(self.time), events, modifiers: self.mods, ..Default::default() };
            let (theme, kb) = (KlippTheme::default(), crate::keybindings::KeyBindings::default());
            let (state, panel, old) = (&mut self.state, &mut self.panel, &mut self.old);
            let mut cmds = Vec::new();
            let out = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                    cmds = panel.show(ui, state, &theme, old, &kb, false);
                });
            });
            // The sequence switch as the app's sequence tabs make it, the
            // focus keeping the selection; a seek, as the app makes it.
            for cmd in &cmds {
                match cmd {
                    AppCommand::SetActiveSequence { sequence_idx } | AppCommand::FocusSequence { sequence_idx } => {
                        let from = self.state.active_sequence_idx;
                        self.state.sequences[from].last_playhead = Some(self.state.playhead);
                        self.state.active_sequence_idx = *sequence_idx;
                        let rate = self.state.sequences[*sequence_idx].fps.rate();
                        self.state.playhead = self.state.sequences[*sequence_idx].last_playhead.unwrap_or(RationalTime::from_frames(0, rate));
                        if matches!(cmd, AppCommand::SetActiveSequence { .. }) {
                            self.state.selected_clip = None;
                            self.state.selected_clips.clear();
                        }
                    }
                    AppCommand::SeekTo(t) => self.state.playhead = *t,
                    _ => {}
                }
            }
            self.sent = cmds;
            self.shapes = out.shapes.clone();
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
            "viewer_transform_drag", "motion_path_drag", "viewer_corner_pin_drag", "viewer_waterline_drag", "viewer_mask_drag", "color_grade_drag_start", "mixer_routing_drag", "resumable_path_drag", "selected_node_id", "selected_node_ids", "selected_node_addr",
            "selected_subtitle_track", "command_palette_selected", "title_find_selected", "workspace_manager_selected", "preset_manager_selected", "show_camera_lane", "file_browser_video_skim_enabled", "file_browser_audio_skim_enabled",
            "save_hyperzoom_preset_name", "save_hyperzoom_preset_error", "save_hyperzoom_preset_source_clip", "hyperzoom_preset_registry", "render_cache_ranges", "render_range", "scope_legal_range",
            "multicam_at_playhead", "waveform_zoom", "viewer_text_edit_select_all", "viewer_text_selection", "viewer_text_selection_locked", "viewer_text_select_anchor",
            "timeline_track_scale", "timeline_prefs", "timeline_search_next", "timeline_search_prev", "browser_skim_texture", "browser_skim_media_id", "skim_transcoding", "skim_in_flight_paths",
            "skim_transcode_status", "skim_transcode_done_at", "browser_skim_pending", "skim_decode_in_flight", "skim_decode_started_at", "skim_queued_request", "timeline_skim_in_flight", "timeline_skim_queued", "timeline_skim_declined", "skim_last_requested", "browser_skim_active_this_frame", "pending_skim_transcodes",
            "clip_carry", "upper_timeline_split", "magnetic_timeline_enabled", "perf_fps", "perf_video_fps",
            "filmstrip_hover_enabled", "transcript_panel_hovered", "speed_dialog_ripple", "ripple_enabled", "linked_selection", "drag_drop", "surface_snaps_pending", "consolidate_include_skims", "project_auto_skim",
        ];
        let per_timeline = ["drag", "select", "hover", "lane", "zoom", "scroll", "playhead", "range", "trim", "key_selection", "keyframe", "fps", "compound", "timeline_", "ruler_", "skim"];
        let mut missing = Vec::new();
        for line in body.lines() {
            let Some(name) = line.strip_prefix("    pub ").and_then(|l| l.split(':').next()) else { continue };
            let looks = per_timeline.iter().any(|w| name.contains(w)) && !name.starts_with("show_");
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
        assert!(!s.sent.iter().any(|c| matches!(c, AppCommand::FocusSequence { .. })));
        s.press(egui::pos2(600.0, 150.0));
        assert!(matches!(s.sent.first(), Some(AppCommand::FocusSequence { sequence_idx: 1 })), "{:?}", s.sent);
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

    /// Each timeline's widgets are its own: a press on the selects' ruler
    /// seeks the selects, one on the cut's seeks the cut.
    #[test]
    fn each_ruler_seeks_its_own_timeline() {
        let mut s = Stack::new();
        let release = |p| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE };
        // The selects' ruler, under their header.
        let top = egui::pos2(500.0, 34.0 + 24.0 + HEADER + 8.0);
        s.press(top);
        s.frame(vec![release(top)]);
        assert_eq!(s.state.active_sequence_idx, 1);
        let selects_at = s.state.playhead;
        assert!(selects_at > Timecode::zero(), "the selects sought");
        // The cut's ruler, under the selects.
        let cut = egui::pos2(700.0, 34.0 + 24.0 + 220.0 + 8.0);
        s.press(cut);
        s.frame(vec![release(cut)]);
        assert_eq!(s.state.active_sequence_idx, 0);
        assert!(s.state.playhead > Timecode::zero(), "the cut sought");
        assert_eq!(s.state.sequences[1].last_playhead, Some(selects_at), "the selects kept theirs");
    }

    /// The selection the selects had parked is theirs again when they
    /// are worked in.
    #[test]
    fn a_focus_keeps_the_selection_it_brings() {
        let mut s = Stack::new();
        let selects_clip = s.state.sequences[1].all_clips().next().unwrap().id;
        s.panel.stacked.as_mut().unwrap().view.selected_clips = vec![selects_clip];
        s.panel.stacked.as_mut().unwrap().view.selected_clip = Some(selects_clip);
        // On the selects' empty track space, right of their clip.
        s.press(egui::pos2(900.0, 34.0 + 24.0 + HEADER + 60.0));
        assert_eq!(s.state.active_sequence_idx, 1);
        assert_eq!(s.state.selected_clip, Some(selects_clip), "{:?}", s.sent);
    }

    /// A track header dragged in the cut, the selects stacked above it,
    /// still reorders the cut's tracks.
    #[test]
    fn the_cut_reorders_its_tracks_with_the_selects_stacked() {
        let mut s = Stack::new();
        // A second video track in the cut, for the first to swap with.
        s.state.sequences[0].tracks.insert(0, klipp_timeline::Track::new("Video 2", TrackKind::Video));
        let texts = s.frame(Vec::new());
        let at = |name: &str| texts.iter().filter(|(t, p)| t == name && p.y > 300.0).map(|(_, p)| *p).next().unwrap_or_else(|| panic!("{name} in the cut: {texts:?}"));
        let (from, to) = (at("Video 2"), at("Video 1"));
        let b = |p, d| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: d, modifiers: egui::Modifiers::NONE };
        s.frame(vec![egui::Event::PointerMoved(from), b(from, true)]);
        let mut sent = Vec::new();
        for i in 1..=6 {
            s.frame(vec![egui::Event::PointerMoved(from + (to - from) * (i as f32 / 6.0))]);
            sent.extend(s.sent.clone());
        }
        s.frame(vec![egui::Event::PointerMoved(to), b(to, false)]);
        sent.extend(s.sent.clone());
        assert!(sent.iter().any(|c| matches!(c, AppCommand::MoveTrack { .. })), "{sent:?}");
    }

    impl Stack {
        /// A drag from `from` to `to` with `mods` held: what was sent on the way.
        fn drag(&mut self, from: egui::Pos2, to: egui::Pos2, mods: egui::Modifiers) -> (Vec<AppCommand>, Vec<String>) {
            self.mods = mods;
            let b = |p, d| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: d, modifiers: mods };
            let mut sent = Vec::new();
            let mut texts = Vec::new();
            self.frame(vec![egui::Event::PointerMoved(from), b(from, true)]);
            sent.extend(self.sent.clone());
            for i in 1..=6 {
                texts.extend(self.frame(vec![egui::Event::PointerMoved(from + (to - from) * (i as f32 / 6.0))]).into_iter().map(|(t, _)| t));
                sent.extend(self.sent.clone());
            }
            self.frame(vec![egui::Event::PointerMoved(to), b(to, false)]);
            sent.extend(self.sent.clone());
            self.mods = egui::Modifiers::NONE;
            (sent, texts)
        }

        /// Where the cut's clip is drawn.
        fn cut_clip(&mut self) -> egui::Pos2 {
            let texts = self.frame(Vec::new());
            texts.iter().find(|(t, p)| t.contains("stack.mov") && p.y > 290.0).unwrap_or_else(|| panic!("the cut's clip: {texts:?}")).1
        }
    }

    /// A Ctrl-drag on a clip carries it, the selection untouched by the
    /// press; let go over its own timeline, nothing at all is done. A
    /// Ctrl-click, no drag, adds the clip to the selection as before.
    #[test]
    fn a_ctrl_drag_carries_and_a_ctrl_click_selects() {
        let mut s = Stack::new();
        let clip = s.state.sequences[0].all_clips().next().unwrap().id;
        let at = s.cut_clip();
        let steps = s.state.active_history().can_undo();
        let (sent, _) = s.drag(at, at + egui::vec2(120.0, 0.0), egui::Modifiers::COMMAND);
        assert_eq!((s.state.selected_clip, s.state.selected_clips.len()), (None, 0), "no toggle from the press");
        assert!(!sent.iter().any(|c| matches!(c, AppCommand::PasteClipsFrom { .. } | AppCommand::MoveClip { .. })), "{sent:?}");
        assert_eq!(s.state.sequences[0].all_clips().next().unwrap().timeline_in, tc(0.0), "not moved");
        assert_eq!(s.state.active_history().can_undo(), steps, "no step");
        assert!(s.state.clip_carry.is_none());
        // A Ctrl-click.
        s.mods = egui::Modifiers::COMMAND;
        let b = |d| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: d, modifiers: egui::Modifiers::COMMAND };
        s.frame(vec![egui::Event::PointerMoved(at), b(true)]);
        s.frame(vec![b(false)]);
        s.mods = egui::Modifiers::NONE;
        assert!(s.state.selected_clip == Some(clip) || s.state.selected_clips.contains(&clip) || s.sent.iter().any(|c| matches!(c, AppCommand::SelectClip(Some(id)) if *id == clip)), "selected: {:?} {:?}", s.state.selected_clip, s.sent);
    }

    /// The cut's clip Ctrl-dragged up onto the selects: a ghost of it
    /// shows where it lands, and let go there the selects are worked in
    /// and the copy is asked for at that time.
    #[test]
    fn a_clip_carried_up_is_copied_into_the_selects() {
        let mut s = Stack::new();
        let clip = s.state.sequences[0].all_clips().next().unwrap().id;
        let from = s.cut_clip();
        let to = egui::pos2(700.0, 34.0 + 24.0 + HEADER + 60.0);
        let (sent, texts) = s.drag(from, to, egui::Modifiers::COMMAND);
        assert!(texts.iter().any(|t| t.starts_with("Insert 1 clip at")), "the ghost: {texts:?}");
        let focus = sent.iter().position(|c| matches!(c, AppCommand::FocusSequence { sequence_idx: 1 }));
        let paste = sent.iter().position(|c| matches!(c, AppCommand::PasteClipsFrom { from, ids, overwrite: false, .. } if *from == s.state.sequences[0].id && ids == &vec![clip]));
        assert!(focus.is_some() && paste.is_some() && focus < paste, "{sent:?}");
    }

    /// The copy a carry asks for, as the app makes it: new ids in the
    /// target, the originals left, one set of edits undo takes back.
    #[test]
    fn a_carried_copy_is_fresh_and_undone_in_one() {
        let s = Stack::new();
        let source = &s.state.sequences[0];
        let ids: Vec<klipp_core::ClipId> = source.all_clips().map(|c| c.id).collect();
        let cb = crate::clip_clipboard::copy(source, &ids).expect("copied");
        let before = s.state.sequences[1].clone();
        let pasted = crate::clip_clipboard::paste(&before, &cb, tc(2.0), crate::clip_clipboard::PasteMode::Insert);
        assert!(!pasted.ids.is_empty() && pasted.ids.iter().all(|id| !ids.contains(id)), "fresh ids");
        let edits = crate::clip_clipboard::track_edits(&before, &pasted.sequence, &pasted.touched, "Insert from the Other Timeline");
        let mut seqs = vec![pasted.sequence.clone()];
        let (mut pool, mut fps) = (s.state.media_pool.clone(), s.state.fps);
        let mut view = klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        for e in edits.iter().rev() {
            view.apply_inverse(e);
        }
        let ids_of = |s: &klipp_timeline::Sequence| s.all_clips().map(|c| c.id).collect::<Vec<_>>();
        assert_eq!(ids_of(&seqs[0]), ids_of(&before), "undone");
    }

    /// A carry left set (let go outside the window, say) is dropped as
    /// soon as no button is down, so a later plain click pastes nothing.
    #[test]
    fn a_stale_carry_is_dropped_and_never_pastes() {
        let mut s = Stack::new();
        let clip = s.state.sequences[0].all_clips().next().unwrap().id;
        s.state.clip_carry = Some((s.state.sequences[0].id, vec![clip]));
        s.frame(Vec::new());
        assert!(s.state.clip_carry.is_none());
        let on = egui::pos2(700.0, 34.0 + 24.0 + HEADER + 60.0);
        let (sent, _) = s.drag(on, on, egui::Modifiers::NONE);
        assert!(!sent.iter().any(|c| matches!(c, AppCommand::PasteClipsFrom { .. })), "{sent:?}");
    }

    /// Carried down onto a track of the cut, the clips go onto that track,
    /// whatever track they were on, and the Linked switch is passed on.
    #[test]
    fn a_carry_lands_on_the_track_it_is_let_go_over() {
        let mut s = Stack::new();
        s.state.sequences[0].tracks.insert(0, klipp_timeline::Track::new("Video 2", TrackKind::Video));
        s.state.linked_selection = false;
        let texts = s.frame(Vec::new());
        let lane_y = texts.iter().find(|(t, p)| t == "Video 2" && p.y > 300.0).expect("the cut's Video 2").1.y;
        let selects_clip = texts.iter().find(|(t, p)| t.contains("stack.mov") && p.y < 280.0).expect("the selects' clip").1;
        let (sent, _) = s.drag(selects_clip, egui::pos2(700.0, lane_y), egui::Modifiers::COMMAND);
        assert!(sent.iter().any(|c| matches!(c, AppCommand::PasteClipsFrom { track: Some(0), linked: false, .. })), "{sent:?}");
    }

    /// With any tool but Select, a Ctrl-drag on a clip is that tool's,
    /// never a carry.
    #[test]
    fn only_the_select_tool_carries() {
        for tool in [klipp_core::Tool::Blade, klipp_core::Tool::Trim, klipp_core::Tool::Range] {
            let mut s = Stack::new();
            s.state.active_tool = tool;
            let at = s.cut_clip();
            let (sent, _) = s.drag(at, egui::pos2(700.0, 34.0 + 24.0 + HEADER + 60.0), egui::Modifiers::COMMAND);
            assert!(!sent.iter().any(|c| matches!(c, AppCommand::PasteClipsFrom { .. })), "{tool:?}: {sent:?}");
            assert!(s.state.clip_carry.is_none());
        }
        let mut s = Stack::new();
        let clip = s.state.sequences[0].all_clips().next().unwrap().id;
        assert!(start_carry(&mut s.state, clip, true), "Select carries");
    }

    /// #2324: with nothing marked, the header's hint is drawn as a plain
    /// label (not a disabled one, which greys it past reading); the
    /// timeline worked in carries the accent bar; the height the upper one
    /// opens at shows its tracks.
    #[test]
    fn the_stack_reads_clearly() {
        let mut s = Stack::new();
        let texts = s.frame(Vec::new());
        assert!(texts.iter().any(|(t, _)| t.starts_with("Mark In and Out")), "the hint drawn: {texts:?}");
        let theme = KlippTheme::default();
        let bars = s.shapes.iter().filter(|c| matches!(&c.shape, egui::Shape::Rect(r) if r.fill == theme.accent && (r.rect.height() - FOCUS_BAR).abs() < 0.01 && r.rect.width() > 100.0)).count();
        assert_eq!(bars, 1, "one accent band, on the timeline worked in");
        let top = s.state.sequences.iter().position(|q| q.name == "Selects").unwrap();
        let want = fitting_height(&s.state, top);
        let tracks: f32 = s.state.sequences[top].tracks.iter().map(crate::panels::timeline::track_row_height).sum();
        assert!(want >= (HEADER + crate::theme::RULER_HEIGHT + tracks).min(MAX_HEIGHT) && want <= MAX_HEIGHT, "{want}");
    }

    /// #2324: the shortcut's request swaps which of the two takes edits,
    /// as a press on the other would, and the band follows; the other's
    /// ruler is veiled.
    #[test]
    fn the_focus_swaps_by_its_shortcut() {
        let mut s = Stack::new();
        let top = s.state.sequences.iter().position(|q| q.name == "Selects").unwrap();
        let before = s.state.active_sequence_idx;
        s.state.stacked_focus_swap = true;
        s.frame(Vec::new());
        assert!(!s.state.stacked_focus_swap, "the request taken");
        assert!(s.sent.iter().any(|c| matches!(c, AppCommand::FocusSequence { sequence_idx } if *sequence_idx == if before == top { 1 - top.min(1) } else { top })), "{:?}", s.sent);
        s.frame(Vec::new());
        let veil = KlippTheme::default().bg_deep.gamma_multiply(0.55);
        assert!(s.shapes.iter().any(|c| matches!(&c.shape, egui::Shape::Rect(r) if r.fill == veil && (r.rect.height() - crate::theme::RULER_HEIGHT).abs() < 0.01)), "the other ruler veiled");
    }
}
