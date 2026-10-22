//! The selects strip (#82): another sequence stacked above the cut,
//! read-only, the pancake way. Skimmed, it shows its picture in the Source
//! viewer and leaves the cut's playhead where it is; a range marked on it
//! (a drag, or a click on a clip) goes into the cut by Insert or
//! Overwrite at the cut's In point (else its playhead), or by dragging it
//! down onto the cut: an insert where it is let go, an overwrite with Alt.
//!
//! The strip is never the timeline being cut: it takes no keys and edits
//! nothing, so a ripple meant for one can't land in the other, and its
//! header says what it is.

use klipp_core::{AppCommand, SequenceId, Timecode};
use klipp_timeline::{ClipKind, Sequence, TrackKind};

use crate::state::AppState;
use crate::theme::KlippTheme;

/// The header row's height.
pub const HEADER: f32 = 24.0;

/// Where the strip draws time: its left edge and points a second, the
/// whole sequence fitted to the width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scale {
    pub left: f32,
    pub px_per_sec: f32,
}

impl Scale {
    /// `seq` fitted into `lanes`, at least ten seconds across.
    pub fn fit(seq: &Sequence, lanes: egui::Rect) -> Self {
        let secs = seq.total_duration().to_secs_f64().max(10.0) as f32;
        Scale { left: lanes.left(), px_per_sec: lanes.width().max(1.0) / secs }
    }

    pub fn x(&self, t: Timecode) -> f32 {
        self.left + t.to_secs_f64() as f32 * self.px_per_sec
    }

    /// The time at `x`, on `rate`'s frames.
    pub fn time(&self, x: f32, rate: u32) -> Timecode {
        Timecode::from_secs_f64((((x - self.left) / self.px_per_sec).max(0.0)) as f64, rate.max(1))
    }
}

/// The range a click on `seq` at `t` takes: the clip there on the highest
/// track that has one.
pub fn clip_range_at(seq: &Sequence, t: Timecode) -> Option<(Timecode, Timecode)> {
    seq.tracks.iter().rev().flat_map(|tr| tr.clips.iter()).find(|c| !matches!(c.kind, ClipKind::Gap) && c.timeline_in <= t && t < c.timeline_out).map(|c| (c.timeline_in, c.timeline_out))
}

/// The picture at `t` in `seq` to skim: the media clip on the highest
/// video track there, and the source second it shows.
pub fn skim_target(seq: &Sequence, t: Timecode) -> Option<(klipp_core::MediaId, f64)> {
    let fps = seq.fps.as_f64();
    seq.tracks.iter().rev().filter(|tr| tr.kind == TrackKind::Video && tr.visible).flat_map(|tr| tr.clips.iter()).find_map(|c| match c.kind {
        ClipKind::Media { media_id } if c.timeline_in <= t && t < c.timeline_out => Some((media_id, c.source_secs_at((t - c.timeline_in).to_secs_f64(), fps))),
        _ => None,
    })
}

/// The selects sequence, if the strip is open on one that isn't the cut.
fn selects(state: &AppState) -> Option<&Sequence> {
    let id = state.selects_sequence?;
    let cut = state.sequence().id;
    state.sequences.iter().find(|s| s.id == id && s.id != cut)
}

/// Draw the strip in `rect` and run it.
pub fn show(ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme, rect: egui::Rect, cmds: &mut Vec<AppCommand>) {
    let Some(seq) = selects(state).cloned() else {
        // Gone, or now the one being cut: the strip closes.
        state.selects_sequence = None;
        state.selects_range = None;
        return;
    };
    let rate = seq.fps.rate();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme.bg_raised);
    painter.hline(rect.x_range(), rect.max.y - 0.5, egui::Stroke::new(1.0_f32, theme.border));

    // ── Header: what it is, its range, and what can be done with it ──
    let header = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), HEADER));
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(header.shrink2(egui::vec2(8.0, 2.0))), |ui| {
        ui.horizontal_centered(|ui| {
            ui.label(egui::RichText::new("SELECTS").size(10.0).strong().color(theme.accent));
            let others: Vec<(SequenceId, String)> = state.sequences.iter().filter(|s| s.id != state.sequence().id).map(|s| (s.id, s.name.clone())).collect();
            let mut chosen = seq.id;
            egui::ComboBox::from_id_salt("selects_sequence").selected_text(&seq.name).width(150.0).show_ui(ui, |ui| {
                for (id, name) in &others {
                    ui.selectable_value(&mut chosen, *id, name);
                }
            });
            if chosen != seq.id {
                state.selects_sequence = Some(chosen);
                state.selects_range = None;
            }
            ui.label(egui::RichText::new("read-only").size(10.0).color(theme.text_dim));
            if ui.small_button("Edit").on_hover_text("Work on the selects as a timeline of their own, above the cut").clicked() {
                state.selects_edit = true;
            }
            ui.separator();
            let range = state.selects_range.filter(|(a, b)| b > a);
            match range {
                Some((a, b)) => ui.label(egui::RichText::new(format!("{} marked", fmt(b - a))).size(11.0).color(theme.text_secondary)),
                None => ui.label(egui::RichText::new("Drag to mark a range, or click a clip").size(11.0).color(theme.text_dim)),
            };
            if let Some((a, b)) = range {
                let edit = |overwrite| AppCommand::EditFromSelects { sequence_id: seq.id, range_in: a, range_out: b, at: None, overwrite };
                if ui.small_button("Insert").on_hover_text("Into the cut at its In point, else its playhead, every track moved along").clicked() {
                    cmds.push(edit(false));
                }
                if ui.small_button("Overwrite").on_hover_text("Over the cut at its In point, else its playhead").clicked() {
                    cmds.push(edit(true));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Close").on_hover_text("Close the selects strip").clicked() {
                    state.selects_sequence = None;
                    state.selects_range = None;
                }
            });
        });
    });
    if state.selects_sequence != Some(seq.id) {
        return;
    }

    // ── Lanes: the sequence fitted to the width ──
    let lanes = egui::Rect::from_min_max(egui::pos2(rect.min.x + 8.0, header.max.y + 2.0), egui::pos2(rect.max.x - 8.0, rect.max.y - 4.0));
    let scale = Scale::fit(&seq, lanes);
    let n = seq.tracks.len().max(1);
    let lane_h = (lanes.height() / n as f32).clamp(6.0, 22.0);
    // The highest track drawn at the top, as the cut draws its picture.
    let lane_rect = |i: usize| {
        let row = n - 1 - i;
        egui::Rect::from_min_size(egui::pos2(lanes.left(), lanes.top() + row as f32 * lane_h), egui::vec2(lanes.width(), lane_h - 1.0))
    };
    for (i, track) in seq.tracks.iter().enumerate() {
        let lane = lane_rect(i);
        let colour = match track.kind {
            TrackKind::Video => theme.accent.gamma_multiply(0.55),
            TrackKind::Audio => egui::Color32::from_rgb(70, 140, 110),
            TrackKind::Text | TrackKind::Subtitle => egui::Color32::from_rgb(150, 110, 170),
        };
        for c in track.clips.iter().filter(|c| !matches!(c.kind, ClipKind::Gap)) {
            let r = egui::Rect::from_min_max(egui::pos2(scale.x(c.timeline_in), lane.top()), egui::pos2(scale.x(c.timeline_out).max(scale.x(c.timeline_in) + 1.0), lane.bottom()));
            painter.rect_filled(r, 2.0, colour);
            if lane_h >= 12.0 && r.width() > 30.0 {
                let name = match &c.kind {
                    ClipKind::Media { media_id } => state.media_pool.get(media_id).map(|m| m.filename.clone()).unwrap_or_default(),
                    ClipKind::Title(_) => "Title".into(),
                    _ => String::new(),
                };
                painter.with_clip_rect(r).text(r.left_center() + egui::vec2(3.0, 0.0), egui::Align2::LEFT_CENTER, name, egui::FontId::proportional(10.0), theme.text_primary);
            }
        }
    }
    if let Some((a, b)) = state.selects_range.filter(|(a, b)| b > a) {
        let r = egui::Rect::from_x_y_ranges(scale.x(a)..=scale.x(b), lanes.y_range());
        painter.rect_filled(r, 0.0, theme.accent.gamma_multiply(0.18));
        painter.rect_stroke(r, 0.0, egui::Stroke::new(1.5_f32, theme.accent));
    }

    // ── Pointer: skim, mark, take a clip, drag the range down ──
    let id = ui.id().with("selects_strip_lanes");
    let response = ui.interact(lanes, id, egui::Sense::click_and_drag());
    let pointer = ui.input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos()));
    if let Some(p) = response.hover_pos() {
        let t = scale.time(p.x, rate);
        painter.vline(p.x, lanes.y_range(), egui::Stroke::new(1.0_f32, theme.text_primary.gamma_multiply(0.5)));
        skim(ui, state, &seq, t, cmds);
    }
    let in_range = |p: egui::Pos2| state.selects_range.is_some_and(|(a, b)| b > a && p.x >= scale.x(a) && p.x <= scale.x(b));
    let mark_from = id.with("mark_from");
    if response.drag_started() {
        // Where the press was, not where the drag was first seen.
        if let Some(p) = ui.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos()) {
            if in_range(p) {
                let (a, b) = state.selects_range.expect("in the range");
                state.selects_drag = Some((seq.id, a, b));
            } else {
                ui.memory_mut(|m| m.data.insert_temp(mark_from, p.x));
            }
        }
    }
    if response.dragged() && state.selects_drag.is_none() {
        if let (Some(from), Some(p)) = (ui.memory(|m| m.data.get_temp::<f32>(mark_from)), pointer) {
            let (a, b) = (scale.time(from.min(p.x), rate), scale.time(from.max(p.x), rate));
            state.selects_range = Some((a, b));
        }
    }
    if response.clicked() {
        if let Some(p) = response.interact_pointer_pos() {
            state.selects_range = clip_range_at(&seq, scale.time(p.x, rate));
        }
    }
    // The range carried down: drawn by the pointer until the cut takes it
    // where it is let go, or it is let go here and goes nowhere.
    if let Some((_, a, b)) = state.selects_drag {
        if !ui.input(|i| i.pointer.any_down()) && response.contains_pointer() {
            state.selects_drag = None;
        } else if let Some(p) = pointer {
            let text = format!("{} from selects", fmt(b - a));
            let ghost = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, id.with("ghost")));
            let galley = ghost.layout_no_wrap(text, egui::FontId::proportional(11.0), theme.text_primary);
            let r = egui::Rect::from_min_size(p + egui::vec2(12.0, 8.0), galley.size() + egui::vec2(8.0, 4.0));
            ghost.rect_filled(r, 3.0, theme.bg_panel);
            ghost.rect_stroke(r, 3.0, egui::Stroke::new(1.0_f32, theme.accent));
            ghost.galley(r.min + egui::vec2(4.0, 2.0), galley, theme.text_primary);
            ui.ctx().request_repaint();
        }
    }
    if !ui.input(|i| i.pointer.any_down()) && !response.contains_pointer() && state.selects_drag.is_some() && !ui.input(|i| i.pointer.any_released()) {
        // Let go over neither the strip nor the cut: dropped nowhere.
        state.selects_drag = None;
    }
}

/// The selects' picture at `t` in the Source viewer, as hovering a clip
/// in the media pool shows it: the cut's playhead stays where it is.
fn skim(ui: &egui::Ui, state: &mut AppState, seq: &Sequence, t: Timecode, cmds: &mut Vec<AppCommand>) {
    if !state.skimming_enabled || state.has_modal_open() {
        return;
    }
    let Some((media_id, secs)) = skim_target(seq, t) else { return };
    state.browser_skim_active_this_frame = true;
    let ready = state.media_pool.get(&media_id).and_then(|m| m.skim_file.as_ref()).is_some_and(|p| p.exists());
    if ready {
        if !state.skim_decode_in_flight {
            cmds.push(AppCommand::BrowserSkimFrame { media_id, time_secs: secs, target: klipp_core::ViewerMode::Source });
        } else {
            state.skim_queued_request = Some((media_id, secs, klipp_core::ViewerMode::Source));
        }
    }
    if state.browser_skim_texture.is_none() {
        if let Some(tex) = state.thumbnail_textures.get(&media_id) {
            state.browser_skim_texture = Some(tex.clone());
        }
    }
    state.browser_skim_media_id = Some(media_id);
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
}

/// A length as minutes and seconds.
fn fmt(t: Timecode) -> String {
    let secs = t.to_secs_f64().max(0.0);
    format!("{}:{:04.1}", (secs / 60.0) as u32, secs % 60.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::Clip;

    fn tc(s: f64) -> Timecode {
        RationalTime::from_secs_f64(s, 25)
    }

    /// A cut and a 20 s selects sequence: shot a 0-8 s, shot b 10-20 s.
    fn state() -> (AppState, SequenceId, [MediaId; 2]) {
        let mut state = AppState::default();
        let media = [MediaId::new(), MediaId::new()];
        let mut selects = Sequence::new_default();
        selects.name = "Selects".into();
        selects.tracks[0].clips = vec![Clip::media(media[0], tc(0.0), tc(8.0), tc(0.0), tc(8.0)), Clip::media(media[1], tc(10.0), tc(20.0), tc(5.0), tc(15.0))];
        let id = selects.id;
        state.sequences.push(selects);
        state.selects_sequence = Some(id);
        (state, id, media)
    }

    const RECT: egui::Rect = egui::Rect { min: egui::pos2(0.0, 0.0), max: egui::pos2(828.0, 120.0) };

    /// A frame of the strip with `events`: what it pushed and drew.
    fn frame(ctx: &egui::Context, state: &mut AppState, events: Vec<egui::Event>) -> (Vec<AppCommand>, Vec<(String, egui::Pos2)>) {
        let mut cmds = Vec::new();
        state.browser_skim_active_this_frame = false;
        let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(828.0, 400.0))), events, ..Default::default() }, |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| show(ui, state, &KlippTheme::default(), RECT, &mut cmds));
        });
        let texts = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
            _ => None,
        }).collect();
        (cmds, texts)
    }

    fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
    }

    /// The x of `secs` in the strip: 20 s across the lanes, 8 to 820.
    fn x(secs: f32) -> f32 {
        8.0 + secs * (812.0 / 20.0)
    }

    /// #82: the strip says it is the selects and read-only; a drag marks a
    /// range, a click takes a clip's, Insert and Overwrite send the range
    /// for the cut's In point; hovering skims its picture in the Source
    /// viewer; Close closes it.
    #[test]
    fn the_strip_marks_skims_and_sends_ranges() {
        let ctx = egui::Context::default();
        let (mut state, id, media) = state();
        let (_, texts) = frame(&ctx, &mut state, Vec::new());
        for t in ["SELECTS", "Selects", "read-only", "Drag to mark a range, or click a clip"] {
            assert!(texts.iter().any(|(s, _)| s == t), "{t}: {:?}", texts.iter().map(|t| &t.0).collect::<Vec<_>>());
        }
        // A drag from 2 s to 6 s marks it.
        let y = 60.0;
        frame(&ctx, &mut state, vec![egui::Event::PointerMoved(egui::pos2(x(2.0), y)), button(egui::pos2(x(2.0), y), true)]);
        for k in 1..=4 {
            frame(&ctx, &mut state, vec![egui::Event::PointerMoved(egui::pos2(x(2.0 + k as f32), y))]);
        }
        frame(&ctx, &mut state, vec![button(egui::pos2(x(6.0), y), false)]);
        let (a, b) = state.selects_range.expect("marked");
        assert!((a.to_secs_f64() - 2.0).abs() < 0.1 && (b.to_secs_f64() - 6.0).abs() < 0.1, "{a:?}..{b:?}");
        // Insert and Overwrite send it, for the cut's In point.
        let (_, texts) = frame(&ctx, &mut state, Vec::new());
        let at = |label: &str| texts.iter().find(|(t, _)| t == label).map(|(_, p)| *p).unwrap_or_else(|| panic!("{label}"));
        let (insert, overwrite) = (at("Insert"), at("Overwrite"));
        let click = |state: &mut AppState, p: egui::Pos2| {
            let mut cmds = frame(&ctx, state, vec![egui::Event::PointerMoved(p), button(p, true)]).0;
            cmds.extend(frame(&ctx, state, vec![button(p, false)]).0);
            cmds
        };
        assert!(matches!(&click(&mut state, insert)[..], [AppCommand::EditFromSelects { sequence_id, range_in, range_out, at: None, overwrite: false }] if *sequence_id == id && *range_in == a && *range_out == b));
        assert!(matches!(&click(&mut state, overwrite)[..], [AppCommand::EditFromSelects { overwrite: true, .. }]));
        // A click on shot b takes its whole range.
        click(&mut state, egui::pos2(x(15.0), y));
        assert_eq!(state.selects_range, Some((tc(10.0), tc(20.0))));
        // Hovering shot b skims it, the cut's playhead untouched.
        let playhead = state.playhead;
        frame(&ctx, &mut state, vec![egui::Event::PointerMoved(egui::pos2(x(12.0), y))]);
        assert!(state.browser_skim_active_this_frame && state.browser_skim_media_id == Some(media[1]));
        assert_eq!(state.playhead, playhead);
        // Close.
        let (_, texts) = frame(&ctx, &mut state, Vec::new());
        let close = texts.iter().find(|(t, _)| t == "Close").map(|(_, p)| *p).unwrap();
        click(&mut state, close);
        assert!(state.selects_sequence.is_none() && state.selects_range.is_none());
    }

    /// #82: a drag from inside the marked range carries it; let go over the
    /// strip, it goes nowhere; the skim target is the highest video
    /// track's picture, at its source time.
    #[test]
    fn a_marked_range_is_carried_and_skim_finds_the_source_time() {
        let ctx = egui::Context::default();
        let (mut state, id, media) = state();
        state.selects_range = Some((tc(10.0), tc(20.0)));
        let y = 60.0;
        // The strip laid out once before it is pressed, as on screen.
        frame(&ctx, &mut state, Vec::new());
        frame(&ctx, &mut state, vec![egui::Event::PointerMoved(egui::pos2(x(14.0), y)), button(egui::pos2(x(14.0), y), true)]);
        for k in 1..=4 {
            frame(&ctx, &mut state, vec![egui::Event::PointerMoved(egui::pos2(x(14.0), y + 20.0 * k as f32))]);
        }
        assert_eq!(state.selects_drag, Some((id, tc(10.0), tc(20.0))), "carried");
        assert_eq!(state.selects_range, Some((tc(10.0), tc(20.0))), "the range unchanged by the carry");
        // Back over the strip and let go: nothing taken.
        frame(&ctx, &mut state, vec![egui::Event::PointerMoved(egui::pos2(x(14.0), y))]);
        frame(&ctx, &mut state, vec![button(egui::pos2(x(14.0), y), false)]);
        frame(&ctx, &mut state, Vec::new());
        assert!(state.selects_drag.is_none());

        let seq = state.sequences.iter().find(|s| s.id == id).unwrap();
        let (m, secs) = skim_target(seq, tc(12.0)).unwrap();
        assert_eq!(m, media[1]);
        assert!((secs - 7.0).abs() < 1e-6, "two seconds into a clip from 5 s: {secs}");
        assert!(skim_target(seq, tc(9.0)).is_none(), "nothing between the shots");
    }
}
