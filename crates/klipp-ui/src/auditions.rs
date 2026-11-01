//! Auditions on the timeline (#3209): a clip holding several takes shows
//! how many on a badge and names the take in its slot; its menu, its keys
//! and an Alt-drop of a file onto it make and grow it; the picker lists
//! its takes to switch, rename, remove or finalize.
//!
//! Every edit is the `alternatives` op, planned by the document as the
//! socket's is, so the app and an assistant make the same change.

use klipp_core::{AppCommand, AuditionEdit, ClipId};
use klipp_document::ops::{AlternativesAction, ClipRef, Op, TakeSpec};
use klipp_timeline::Clip;

/// The op `edit` is on `clip`.
pub fn op_for(clip: &Clip, edit: &AuditionEdit) -> Op {
    let op = |action, takes: Vec<TakeSpec>, take: Option<String>, name: Option<String>, ripple| Op::Alternatives { action, clip_id: ClipRef::Id(clip.id), takes, take, name, ripple };
    let media = |ids: &[klipp_core::MediaId]| ids.iter().map(|&id| TakeSpec { media_id: Some(id), ..Default::default() }).collect();
    match edit {
        AuditionEdit::Create => op(AlternativesAction::Create, Vec::new(), None, None, false),
        // Duplicate as Audition: a copy to vary, the audition made first
        // when the clip is not one.
        AuditionEdit::AddCopy if clip.audition.is_none() => op(AlternativesAction::Create, Vec::new(), None, None, false),
        AuditionEdit::AddCopy => op(AlternativesAction::Add, vec![TakeSpec::default()], None, None, false),
        AuditionEdit::AddMedia { media_ids } if clip.audition.is_none() => op(AlternativesAction::Create, media(media_ids), None, None, false),
        AuditionEdit::AddMedia { media_ids } => op(AlternativesAction::Add, media(media_ids), None, None, false),
        AuditionEdit::Switch { take, ripple } => op(AlternativesAction::Switch, Vec::new(), Some(take.to_string()), None, *ripple),
        AuditionEdit::Rename { take, name } => op(AlternativesAction::Rename, Vec::new(), Some(take.to_string()), Some(name.clone()), false),
        AuditionEdit::Remove { take } => op(AlternativesAction::Remove, Vec::new(), Some(take.to_string()), None, false),
        AuditionEdit::Finalize => op(AlternativesAction::Finalize, Vec::new(), None, None, false),
    }
}

/// The edits `edit` makes on `clip_id` in `seq`, with what the step is
/// called; why not, when refused.
pub fn plan(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool, clip_id: ClipId, edit: &AuditionEdit) -> Result<(Vec<klipp_document::history::EditCommand>, String), String> {
    let clip = seq.all_clips().find(|c| c.id == clip_id).ok_or("the clip is no longer in the sequence")?;
    let plan = klipp_document::ops::resolve(seq, pool, &[op_for(clip, edit)]).map_err(|r| r.why)?;
    let label = match edit {
        AuditionEdit::Create => "Create Audition",
        AuditionEdit::AddCopy | AuditionEdit::AddMedia { .. } => if clip.audition.is_none() { "Create Audition" } else { "Add to Audition" },
        AuditionEdit::Switch { .. } => "Switch Take",
        AuditionEdit::Rename { .. } => "Rename Take",
        AuditionEdit::Remove { .. } => "Remove from Audition",
        AuditionEdit::Finalize => "Finalize Audition",
    };
    Ok((plan.steps.into_iter().map(|s| s.edit).collect(), label.to_string()))
}

/// How far before and after its slot a take is played when previewed.
pub const PREVIEW_ROLL_SECS: f64 = 2.0;

/// `seq` with `take` in `clip_id`'s slot (#3209), as switching to it
/// would make it, the clips after it moved as a switch moves them: what a
/// preview of the take plays. `seq` itself, unchanged, for the take
/// already in the slot.
pub fn take_sequence(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool, clip_id: ClipId, take: klipp_core::TakeId, ripple: bool) -> Result<klipp_timeline::Sequence, String> {
    let clip = seq.all_clips().find(|c| c.id == clip_id).ok_or("the clip is no longer in the sequence")?;
    let audition = clip.audition.as_deref().ok_or("the clip is not an audition")?;
    if audition.active == take {
        return Ok(seq.clone());
    }
    let (edits, _) = plan(seq, pool, clip_id, &AuditionEdit::Switch { take, ripple })?;
    let mut sequences = vec![seq.clone()];
    let mut fps = seq.fps;
    // The edits are of the sequence alone: the pool beside them is a
    // stand-in, never read and dropped after.
    let mut scratch = klipp_media::MediaPool::default();
    let mut view = klipp_document::apply::DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut scratch, fps: &mut fps };
    for e in &edits {
        view.apply_forward(e);
    }
    Ok(sequences.remove(0))
}

/// The time a take of `clip_id` plays over in `seq` (its preview's
/// sequence): its slot and [`PREVIEW_ROLL_SECS`] either side, from 0.
pub fn preview_range(seq: &klipp_timeline::Sequence, clip_id: ClipId) -> Option<(klipp_core::Timecode, klipp_core::Timecode)> {
    let clip = seq.all_clips().find(|c| c.id == clip_id)?;
    let rate = clip.timeline_in.rate;
    let from = (clip.timeline_in.to_secs_f64() - PREVIEW_ROLL_SECS).max(0.0);
    let to = clip.timeline_out.to_secs_f64() + PREVIEW_ROLL_SECS;
    Some((klipp_core::Timecode::from_secs_f64(from, rate), klipp_core::Timecode::from_secs_f64(to, rate)))
}

/// A take preview's takes in the order they are stepped through, and the
/// one to start on: the one asked for, else the one in the slot.
pub fn preview_order(audition: &klipp_timeline::audition::Audition, start: Option<klipp_core::TakeId>) -> (Vec<klipp_core::TakeId>, usize) {
    let order: Vec<klipp_core::TakeId> = audition.takes.iter().map(|t| t.id).collect();
    let at = start.or(Some(audition.active)).and_then(|id| order.iter().position(|t| *t == id)).unwrap_or(0);
    (order, at)
}

/// The take `by` steps from `index` among `len`, round from the last to
/// the first and back.
pub fn stepped(index: usize, len: usize, by: i32) -> usize {
    if len == 0 {
        return 0;
    }
    (index as i64 + i64::from(by)).rem_euclid(len as i64) as usize
}

/// An audition's takes being played in its slot (#3209): the engine is
/// handed the sequence with the take in it in place of the document,
/// which is kept beside it and given back when it ends.
#[derive(Debug, Clone, PartialEq)]
pub struct TakePreview {
    pub clip_id: ClipId,
    /// The takes, in the order they are stepped through.
    pub order: Vec<klipp_core::TakeId>,
    pub index: usize,
    /// Each take in turn, not the one round and round.
    pub all: bool,
    /// The time the take plays over, in its own sequence.
    pub range: (klipp_core::Timecode, klipp_core::Timecode),
    /// Where the playhead was, to go back to.
    pub return_to: klipp_core::Timecode,
}

impl TakePreview {
    pub fn take(&self) -> Option<klipp_core::TakeId> {
        self.order.get(self.index).copied()
    }
}

/// The commands a take preview stays up through: the transport, the
/// selection, the panels, and its own. Anything else ends it first, so it
/// sees and changes the document.
pub fn keeps_take_preview(cmd: &AppCommand) -> bool {
    matches!(
        cmd,
        AppCommand::Play
            | AppCommand::Pause
            | AppCommand::SeekTo(_)
            | AppCommand::SeekRelative(_)
            | AppCommand::SequenceUpdated
            | AppCommand::SelectClip(_)
            | AppCommand::OpenAuditionPicker { .. }
            | AppCommand::PreviewTakes { .. }
            | AppCommand::StepTakePreview { .. }
            | AppCommand::PickPreviewedTake
            | AppCommand::EndTakePreview
            | AppCommand::ShowPanel { .. }
    )
}

/// A preview of `clip_id`'s takes started, from `take`: its commands.
pub fn start_preview(state: &mut crate::state::AppState, shared: &klipp_timeline::SharedSequence, clip_id: ClipId, take: Option<klipp_core::TakeId>, all: bool) -> Vec<AppCommand> {
    let Some(audition) = state.sequence().all_clips().find(|c| c.id == clip_id).and_then(|c| c.audition.as_deref().cloned()) else {
        state.toast.show("Not an audition: nothing to preview".to_string());
        return Vec::new();
    };
    let (order, index) = preview_order(&audition, take);
    // One already running keeps the playhead it came from.
    let return_to = state.take_preview.as_ref().map_or(state.playhead, |p| p.return_to);
    let range = (state.playhead, state.playhead);
    state.take_preview = Some(TakePreview { clip_id, order, index, all, range, return_to });
    show_take(state, shared)
}

/// The take the preview is on handed to the engine in place of the
/// document, and played from the start of its range: the commands that
/// do it. A take that can't be shown ends the preview, said in a toast.
pub fn show_take(state: &mut crate::state::AppState, shared: &klipp_timeline::SharedSequence) -> Vec<AppCommand> {
    let Some(p) = state.take_preview.clone() else { return Vec::new() };
    let Some(take) = p.take() else { return Vec::new() };
    // Made from the document, never from a take already standing in.
    let made = take_sequence(state.sequence(), &state.media_pool, p.clip_id, take, state.ripple_enabled)
        .and_then(|seq| preview_range(&seq, p.clip_id).map(|r| (seq, r)).ok_or_else(|| "the clip is no longer in the sequence".to_string()));
    match made {
        Ok((seq, range)) => {
            shared.stand_in(seq, klipp_timeline::StandInFor { clip: p.clip_id, take });
            if let Some(now) = state.take_preview.as_mut() {
                now.range = range;
            }
            vec![AppCommand::SequenceUpdated, AppCommand::SeekTo(range.0), AppCommand::Play]
        }
        Err(why) => {
            state.toast.show(format!("The take can't be previewed: {why}"));
            end_preview(state, shared, true)
        }
    }
}

/// The take preview ended: the engine given the document back as it was,
/// the playback paused and, with `go_back`, the playhead where it was.
pub fn end_preview(state: &mut crate::state::AppState, shared: &klipp_timeline::SharedSequence, go_back: bool) -> Vec<AppCommand> {
    let Some(p) = state.take_preview.take() else { return Vec::new() };
    let mut cmds = vec![AppCommand::Pause];
    if shared.end_stand_in() {
        cmds.push(AppCommand::SequenceUpdated);
    }
    if go_back {
        cmds.push(AppCommand::SeekTo(p.return_to));
    }
    cmds
}

/// Enter on a take preview: it ended, and the take playing put in the slot
/// when it is not the one there.
pub fn pick_previewed(state: &mut crate::state::AppState, shared: &klipp_timeline::SharedSequence) -> Vec<AppCommand> {
    let picked = state.take_preview.as_ref().and_then(|p| p.take().map(|t| (p.clip_id, t)));
    let mut cmds = end_preview(state, shared, false);
    if let Some((clip_id, take)) = picked {
        let active = state.sequence().all_clips().find(|c| c.id == clip_id).and_then(|c| c.audition.as_deref().map(|a| a.active));
        if active != Some(take) {
            cmds.push(AppCommand::EditAudition { clip_id, edit: AuditionEdit::Switch { take, ripple: state.ripple_enabled } });
        }
    }
    cmds
}

/// Once a frame while a take is previewed: past the end of its range it
/// plays the next take (with `all`; after the last it ends), or the same
/// one again. Ended elsewhere (a change from the socket gives the engine
/// the document), it is let go.
pub fn follow_preview(state: &mut crate::state::AppState, shared: &klipp_timeline::SharedSequence) -> Vec<AppCommand> {
    let Some(p) = state.take_preview.clone() else { return Vec::new() };
    if shared.standing_in().is_none() {
        state.take_preview = None;
        return vec![AppCommand::Pause, AppCommand::SequenceUpdated];
    }
    if state.playback != klipp_engine::PlaybackState::Playing || state.playhead < p.range.1 {
        return Vec::new();
    }
    if !p.all {
        return vec![AppCommand::SeekTo(p.range.0)];
    }
    if p.index + 1 >= p.order.len() {
        return end_preview(state, shared, true);
    }
    if let Some(now) = state.take_preview.as_mut() {
        now.index += 1;
    }
    show_take(state, shared)
}

/// Whether `clip` can hold takes: what the op makes an audition of.
pub fn can_hold_takes(clip: &Clip) -> bool {
    clip.audition.is_some() || matches!(clip.kind, klipp_timeline::ClipKind::Media { .. } | klipp_timeline::ClipKind::Title(_) | klipp_timeline::ClipKind::Generator(_) | klipp_timeline::ClipKind::Sequence { .. })
}

/// The clip of `track` at `secs` an Alt-drop adds the files to as takes
/// (#3209): one that can hold takes, on a track not locked.
pub fn drop_target(track: &klipp_timeline::Track, secs: f64) -> Option<ClipId> {
    if track.locked {
        return None;
    }
    track.clips.iter().find(|c| c.timeline_in.to_secs_f64() <= secs && secs < c.timeline_out.to_secs_f64() && can_hold_takes(c)).map(|c| c.id)
}

/// `label` with the name of the take in the slot, for an audition: what
/// the clip shows on the timeline.
pub fn label_with_take(clip: &Clip, label: &str) -> String {
    match clip.audition.as_deref().and_then(|a| a.active_take()) {
        Some(t) if !t.name.is_empty() => format!("{label} \u{00b7} {}", t.name),
        _ => label.to_string(),
    }
}

/// Where an audition's badge sits on its clip: the top-left corner, as
/// many takes as it holds. `None` for a clip that is not one, or too
/// narrow to show it.
pub fn badge_rect(clip: &Clip, clip_rect: egui::Rect) -> Option<egui::Rect> {
    (clip.audition.is_some() && clip_rect.width() > 30.0).then(|| egui::Rect::from_min_size(clip_rect.left_top() + egui::vec2(3.0, 3.0), egui::vec2(22.0, 14.0)))
}

/// The badge drawn: a spotlight's count of takes.
pub fn draw_badge(painter: &egui::Painter, clip: &Clip, clip_rect: egui::Rect, fill: egui::Color32, text: egui::Color32) {
    let (Some(rect), Some(a)) = (badge_rect(clip, clip_rect), clip.audition.as_deref()) else { return };
    painter.rect_filled(rect, 3.0, fill);
    painter.text(rect.center(), egui::Align2::CENTER_CENTER, format!("\u{25B8}{}", a.takes.len()), egui::FontId::proportional(10.0), text);
}

/// The picker for `state.audition_picker`'s clip: its takes, the one in
/// the slot marked; a click on another puts it in the slot; each can be
/// renamed, and removed but the one in the slot; Add Copy and Finalize.
/// Its commands.
pub fn show_picker(ctx: &egui::Context, state: &mut crate::state::AppState) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let Some(clip_id) = state.audition_picker else { return cmds };
    let Some(clip) = state.sequence().all_clips().find(|c| c.id == clip_id).cloned() else {
        state.audition_picker = None;
        return cmds;
    };
    let Some(a) = clip.audition.as_deref().cloned() else {
        state.audition_picker = None;
        return cmds;
    };
    let ripple = state.ripple_enabled;
    let fps = state.sequence().fps.rate();
    let mut open = true;
    egui::Window::new("Takes")
        .id(egui::Id::new(("audition_picker", clip_id.0)))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(360.0)
        .show(ctx, |ui| {
            ui.label(egui::RichText::new(format!("{} takes in this slot. Click one to put it in.", a.takes.len())).weak());
            egui::Grid::new(("audition_takes", clip_id.0)).num_columns(4).striped(true).show(ui, |ui| {
                for t in &a.takes {
                    let active = t.id == a.active;
                    let shown = t.clip.as_deref().unwrap_or(&clip);
                    let secs = (shown.timeline_out - shown.timeline_in).to_secs_f64();
                    // The name being typed is kept across frames until the
                    // field is left, which renames the take.
                    let field = egui::Id::new(("take_name", t.id.to_string()));
                    let typed = field.with("typed");
                    let mut name: String = ui.data_mut(|d| d.get_temp(typed)).unwrap_or_else(|| t.name.clone());
                    let edit = ui.add(egui::TextEdit::singleline(&mut name).desired_width(140.0).id(field));
                    if edit.lost_focus() {
                        ui.data_mut(|d| d.remove::<String>(typed));
                        if name.trim() != t.name && !name.trim().is_empty() {
                            cmds.push(AppCommand::EditAudition { clip_id, edit: AuditionEdit::Rename { take: t.id, name: name.trim().to_string() } });
                        }
                    } else if edit.has_focus() {
                        ui.data_mut(|d| d.insert_temp(typed, name));
                    }
                    let length = klipp_core::Timecode::from_secs_f64(secs, fps);
                    if ui.selectable_label(active, format!("{}  {}", if active { "\u{25CF} In the slot" } else { "Put in" }, state.timecode_display().duration(length))).on_hover_text(if active { "The take on the timeline" } else { "Put this take in the slot: one undo step" }).clicked() && !active {
                        cmds.push(AppCommand::EditAudition { clip_id, edit: AuditionEdit::Switch { take: t.id, ripple } });
                    }
                    let playing = state.take_preview.as_ref().is_some_and(|p| p.clip_id == clip_id && p.take() == Some(t.id));
                    if ui.selectable_label(playing, "\u{25B6}").on_hover_text("Play this take in its slot, round and round, nothing changed").clicked() {
                        cmds.push(AppCommand::PreviewTakes { clip_id, take: Some(t.id), all: false });
                    }
                    if ui.add_enabled(!active, egui::Button::new("Remove")).on_disabled_hover_text("Put another take in first").clicked() {
                        cmds.push(AppCommand::EditAudition { clip_id, edit: AuditionEdit::Remove { take: t.id } });
                    }
                    ui.end_row();
                }
            });
            if state.take_preview.as_ref().is_some_and(|p| p.clip_id == clip_id) {
                ui.label(egui::RichText::new("Playing in the slot: \u{2190} \u{2192} another take, Enter puts it in, Esc stops").weak());
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Preview All").on_hover_text("Play each take in its slot in turn, a little either side, nothing changed").clicked() {
                    cmds.push(AppCommand::PreviewTakes { clip_id, take: a.takes.first().map(|t| t.id), all: true });
                }
                if ui.button("Add Copy").on_hover_text("A copy of the take in the slot, to vary").clicked() {
                    cmds.push(AppCommand::EditAudition { clip_id, edit: AuditionEdit::AddCopy });
                }
                if ui.button("Finalize").on_hover_text("Keep the take in the slot as a plain clip and drop the others").clicked() {
                    cmds.push(AppCommand::EditAudition { clip_id, edit: AuditionEdit::Finalize });
                    cmds.push(AppCommand::OpenAuditionPicker { clip_id: None });
                }
            });
        });
    if !open {
        state.audition_picker = None;
        if state.take_preview.is_some() {
            cmds.push(AppCommand::EndTakePreview);
        }
    }
    cmds
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::RationalTime;
    use klipp_timeline::{Sequence, TrackKind};

    fn t(secs: f64) -> RationalTime {
        RationalTime::from_secs_f64(secs, 24)
    }

    /// A sequence with a 4 s clip on its first video track, and a second
    /// file in the pool.
    fn fixture() -> (Sequence, klipp_media::MediaPool, ClipId, klipp_core::MediaId) {
        let mut pool = klipp_media::MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/wide.mov".into(), None);
        item.duration = t(20.0);
        let wide = pool.add(item);
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/close.mov".into(), None);
        item.duration = t(20.0);
        let close = pool.add(item);
        let mut seq = Sequence::new_default();
        let v = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
        let clip = Clip::media(wide, t(0.0), t(4.0), t(0.0), t(4.0));
        let id = clip.id;
        seq.tracks[v].clips.push(clip);
        (seq, pool, id, close)
    }

    /// `edit` planned and applied to `seq`.
    fn apply(seq: &mut Sequence, pool: &mut klipp_media::MediaPool, id: ClipId, edit: AuditionEdit) -> String {
        let (edits, label) = plan(seq, pool, id, &edit).unwrap_or_else(|e| panic!("{edit:?}: {e}"));
        let mut seqs = vec![seq.clone()];
        let mut fps = seq.fps;
        let mut view = klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: pool, fps: &mut fps };
        for e in &edits {
            view.apply_forward(e);
        }
        *seq = seqs.remove(0);
        label
    }

    fn audition(seq: &Sequence, id: ClipId) -> klipp_timeline::audition::Audition {
        seq.all_clips().find(|c| c.id == id).and_then(|c| c.audition.as_deref().cloned()).expect("an audition")
    }

    /// #3209: what the app's menu, keys and drop ask for is the
    /// alternatives op's edit: Create makes the clip and a copy; an Alt-drop
    /// of a file on a plain clip makes it an audition with the file as a
    /// take, and on an audition adds it; Duplicate as Audition adds a copy;
    /// Switch, Rename, Remove and Finalize as the op does them; each step
    /// named for the history.
    #[test]
    fn the_apps_audition_edits_are_the_ops() {
        let (mut seq, mut pool, id, close) = fixture();
        assert_eq!(apply(&mut seq, &mut pool, id, AuditionEdit::Create), "Create Audition");
        let a = audition(&seq, id);
        assert_eq!(a.takes.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Take 1", "Take 2"]);
        assert_eq!(apply(&mut seq, &mut pool, id, AuditionEdit::AddMedia { media_ids: vec![close] }), "Add to Audition");
        assert_eq!(apply(&mut seq, &mut pool, id, AuditionEdit::AddCopy), "Add to Audition");
        let a = audition(&seq, id);
        assert_eq!(a.takes.len(), 4);
        let file_take = a.takes[2].id;
        assert_eq!(a.takes[2].clip.as_ref().and_then(|c| c.media_id()), Some(close), "the dropped file's take");

        apply(&mut seq, &mut pool, id, AuditionEdit::Switch { take: file_take, ripple: false });
        let slot = seq.all_clips().find(|c| c.id == id).unwrap();
        assert_eq!(slot.media_id(), Some(close), "the file's take in the slot, the slot's id kept");
        assert_eq!(label_with_take(slot, "close.mov"), "close.mov \u{00b7} Take 3");
        apply(&mut seq, &mut pool, id, AuditionEdit::Rename { take: file_take, name: "Close-up".into() });
        assert_eq!(audition(&seq, id).active_take().unwrap().name, "Close-up");
        let first = audition(&seq, id).takes[0].id;
        assert!(plan(&seq, &pool, id, &AuditionEdit::Remove { take: file_take }).is_err(), "the take in the slot is not removed");
        apply(&mut seq, &mut pool, id, AuditionEdit::Remove { take: first });
        assert_eq!(audition(&seq, id).takes.len(), 3);
        assert_eq!(apply(&mut seq, &mut pool, id, AuditionEdit::Finalize), "Finalize Audition");
        let slot = seq.all_clips().find(|c| c.id == id).unwrap();
        assert!(slot.audition.is_none() && slot.media_id() == Some(close), "a plain clip of the take kept");

        // A file dropped on a plain clip makes the audition with it.
        let (mut seq, mut pool, id, close) = fixture();
        assert_eq!(apply(&mut seq, &mut pool, id, AuditionEdit::AddMedia { media_ids: vec![close] }), "Create Audition");
        assert_eq!(audition(&seq, id).takes.len(), 2);
        assert_eq!(audition(&seq, id).takes[1].clip.as_ref().and_then(|c| c.media_id()), Some(close));
    }

    type Drawn = (Vec<AppCommand>, Vec<(String, egui::Pos2)>);

    /// A click on the text starting `label`, pressed in a frame and let
    /// go in the next: what either sent.
    fn click(frame: &mut impl FnMut(&mut crate::state::AppState, Vec<egui::Event>) -> Drawn, state: &mut crate::state::AppState, label: &str) -> Vec<AppCommand> {
        // A window is laid out unseen in its first frame.
        frame(state, Vec::new());
        let (_, texts) = frame(state, Vec::new());
        let at = texts.iter().find(|(t, _)| t.starts_with(label)).map(|(_, p)| *p).unwrap_or_else(|| panic!("no {label:?}: {texts:?}"));
        let mut cmds = Vec::new();
        for pressed in [true, false] {
            cmds.extend(frame(state, vec![egui::Event::PointerMoved(at), egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]).0);
        }
        cmds
    }

    /// #3209's picker: each take listed, the one in the slot marked and
    /// not removable; a click on another puts it in the slot (with the
    /// timeline's ripple); Remove, Add Copy and Finalize send their edits;
    /// Finalize closes it.
    #[test]
    fn the_picker_lists_the_takes_and_switches_to_one() {
        let (seq, pool, id, _) = fixture();
        let mut state = crate::state::AppState::default();
        state.sequences[0] = seq;
        state.media_pool = pool;
        let (edits, _) = plan(state.sequence(), &state.media_pool, id, &AuditionEdit::Create).unwrap();
        let mut fps = state.fps;
        let mut view = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: 0, media_pool: &mut state.media_pool, fps: &mut fps };
        for e in &edits {
            view.apply_forward(e);
        }
        state.audition_picker = Some(id);
        state.ripple_enabled = true;
        let second = audition(state.sequence(), id).takes[1].id;
        let ctx = egui::Context::default();
        let mut clock = 0.0;
        let mut frame = |state: &mut crate::state::AppState, events: Vec<egui::Event>| -> Drawn {
            clock += 0.1;
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 700.0))), time: Some(clock), events, ..Default::default() };
            let mut cmds = Vec::new();
            let out = ctx.run(input, |ctx| cmds = show_picker(ctx, state));
            let texts: Vec<(String, egui::Pos2)> = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.visual_bounding_rect().center())),
                _ => None,
            }).collect();
            (cmds, texts)
        };
        let cmds = click(&mut frame, &mut state, "Put in");
        assert!(matches!(&cmds[..], [AppCommand::EditAudition { clip_id, edit: AuditionEdit::Switch { take, ripple: true } }] if *clip_id == id && *take == second), "{cmds:?}");
        assert!(click(&mut frame, &mut state, "\u{25CF} In the slot").is_empty(), "the take in the slot is not switched to");
        // A take renamed: its field clicked, a name typed a key a frame,
        // then a click elsewhere.
        let cmds = click(&mut frame, &mut state, "Take 2");
        assert!(cmds.is_empty());
        let key = |key| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() };
        frame(&mut state, vec![key(egui::Key::End)]);
        for _ in 0..8 {
            frame(&mut state, vec![key(egui::Key::Backspace)]);
        }
        let mut cmds = Vec::new();
        for c in ["W", "i", "d", "e"] {
            cmds.extend(frame(&mut state, vec![egui::Event::Text(c.into())]).0);
        }
        assert!(cmds.is_empty(), "nothing renamed while typing: {cmds:?}");
        let away = egui::pos2(880.0, 680.0);
        for pressed in [true, false] {
            cmds.extend(frame(&mut state, vec![egui::Event::PointerMoved(away), egui::Event::PointerButton { pos: away, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]).0);
        }
        assert!(matches!(&cmds[..], [AppCommand::EditAudition { edit: AuditionEdit::Rename { take, name }, .. }] if *take == second && name == "Wide"), "{cmds:?}");
        let cmds = click(&mut frame, &mut state, "Add Copy");
        assert!(matches!(&cmds[..], [AppCommand::EditAudition { edit: AuditionEdit::AddCopy, .. }]), "{cmds:?}");
        let cmds = click(&mut frame, &mut state, "Finalize");
        assert!(matches!(&cmds[..], [AppCommand::EditAudition { edit: AuditionEdit::Finalize, .. }, AppCommand::OpenAuditionPicker { clip_id: None }]), "{cmds:?}");
    }

    fn dbg_of(cmds: &[AppCommand]) -> Vec<String> {
        cmds.iter().map(|c| format!("{c:?}")).collect()
    }

    /// #3209 PR 3: a take previewed is handed to the engine in place of
    /// the document, which stays as it was, and so do its history and
    /// whether it is saved; played through, each take follows the last
    /// and the preview ends after the last, the document handed back and
    /// the playhead where it was; one take plays round and round; Enter
    /// ends it and puts the take in, one edit; a change from elsewhere
    /// ends it.
    #[test]
    fn a_take_preview_plays_the_take_and_leaves_the_project_untouched() {
        let (seq, pool, id, close) = fixture();
        let mut state = crate::state::AppState::default();
        state.sequences[0] = seq;
        state.media_pool = pool;
        for edit in [AuditionEdit::Create, AuditionEdit::AddMedia { media_ids: vec![close] }] {
            let (edits, _) = plan(state.sequence(), &state.media_pool, id, &edit).unwrap();
            let mut fps = state.fps;
            let mut view = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: 0, media_pool: &mut state.media_pool, fps: &mut fps };
            for e in &edits {
                view.apply_forward(e);
            }
        }
        state.dirty = false;
        state.playhead = t(10.0);
        let document = state.sequence().clone();
        let shared = klipp_timeline::SharedSequence::new(document.clone());
        let takes: Vec<_> = audition(&document, id).takes.iter().map(|t| t.id).collect();
        let media_in_slot = |s: &Sequence| s.all_clips().find(|c| c.id == id).and_then(|c| c.media_id());
        let wide = media_in_slot(&document);

        // The file's take, the last of three, played through.
        let cmds = start_preview(&mut state, &shared, id, Some(takes[2]), true);
        assert_eq!(dbg_of(&cmds), dbg_of(&[AppCommand::SequenceUpdated, AppCommand::SeekTo(t(0.0)), AppCommand::Play]), "from 2 s before the slot, clamped to 0");
        assert_eq!(state.take_preview.as_ref().unwrap().range, (t(0.0), t(6.0)), "the slot and 2 s after it");
        assert_eq!(media_in_slot(&shared.snapshot()), Some(close), "the engine plays the take");
        assert_eq!(media_in_slot(&shared.document()), wide, "the document beside it as it was");
        assert_eq!(format!("{:?}", state.sequence()), format!("{document:?}"), "the app's document untouched");
        assert!(!state.dirty && state.next_undo().is_none(), "nothing to save, nothing to undo");
        assert_eq!(shared.standing_in(), Some(klipp_timeline::StandInFor { clip: id, take: takes[2] }));

        // Before its end nothing happens; at it, the last take, it ends.
        state.playback = klipp_engine::PlaybackState::Playing;
        state.playhead = t(5.0);
        assert!(follow_preview(&mut state, &shared).is_empty());
        state.playhead = t(6.0);
        assert_eq!(dbg_of(&follow_preview(&mut state, &shared)), dbg_of(&[AppCommand::Pause, AppCommand::SequenceUpdated, AppCommand::SeekTo(t(10.0))]), "back where the playhead was");
        assert!(state.take_preview.is_none() && shared.standing_in().is_none());
        assert_eq!(format!("{:?}", shared.snapshot()), format!("{document:?}"), "the document handed back as it was");

        // Played through from the first, the second follows it.
        start_preview(&mut state, &shared, id, Some(takes[0]), true);
        state.playhead = t(6.0);
        follow_preview(&mut state, &shared);
        assert_eq!(state.take_preview.as_ref().and_then(TakePreview::take), Some(takes[1]));
        end_preview(&mut state, &shared, true);

        // One take, round and round.
        start_preview(&mut state, &shared, id, Some(takes[2]), false);
        state.playhead = t(6.0);
        assert_eq!(dbg_of(&follow_preview(&mut state, &shared)), dbg_of(&[AppCommand::SeekTo(t(0.0))]));
        assert_eq!(stepped(0, 3, -1), 2, "\u{2190} from the first is the last");
        assert_eq!(stepped(2, 3, 1), 0);

        // Enter: ended, and the take put in, one edit.
        let cmds = pick_previewed(&mut state, &shared);
        assert!(matches!(&cmds[..], [AppCommand::Pause, AppCommand::SequenceUpdated, AppCommand::EditAudition { edit: AuditionEdit::Switch { take, .. }, .. }] if *take == takes[2]), "{cmds:?}");
        assert!(shared.standing_in().is_none());

        // A change from elsewhere (the socket) ends it.
        start_preview(&mut state, &shared, id, Some(takes[2]), false);
        shared.write().unwrap().name = "edited over the socket".into();
        assert_eq!(dbg_of(&follow_preview(&mut state, &shared)), dbg_of(&[AppCommand::Pause, AppCommand::SequenceUpdated]));
        assert!(state.take_preview.is_none());

        // What the preview stays up through, and what ends it.
        assert!(keeps_take_preview(&AppCommand::Play) && keeps_take_preview(&AppCommand::SeekTo(t(1.0))));
        for ends in [AppCommand::Undo, AppCommand::Redo, AppCommand::SetActiveSequence { sequence_idx: 0 }, AppCommand::EditAudition { clip_id: id, edit: AuditionEdit::Finalize }, AppCommand::Stop] {
            assert!(!keeps_take_preview(&ends), "{ends:?} ends it");
        }
    }

    /// The drop's target is the clip under the pointer that can hold
    /// takes, on a track not locked.
    #[test]
    fn an_alt_drop_goes_to_the_clip_under_it() {
        let (mut seq, _, id, _) = fixture();
        let v = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
        assert_eq!(drop_target(&seq.tracks[v], 2.0), Some(id));
        assert_eq!(drop_target(&seq.tracks[v], 5.0), None, "past the clip");
        seq.tracks[v].locked = true;
        assert_eq!(drop_target(&seq.tracks[v], 2.0), None, "a locked track");
    }
}
