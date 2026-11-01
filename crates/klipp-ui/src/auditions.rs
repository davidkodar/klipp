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
            egui::Grid::new(("audition_takes", clip_id.0)).num_columns(3).striped(true).show(ui, |ui| {
                for t in &a.takes {
                    let active = t.id == a.active;
                    let shown = t.clip.as_deref().unwrap_or(&clip);
                    let secs = (shown.timeline_out - shown.timeline_in).to_secs_f64();
                    let mut name = t.name.clone();
                    let edit = ui.add(egui::TextEdit::singleline(&mut name).desired_width(140.0).id(egui::Id::new(("take_name", t.id.to_string()))));
                    if edit.lost_focus() && name.trim() != t.name && !name.trim().is_empty() {
                        cmds.push(AppCommand::EditAudition { clip_id, edit: AuditionEdit::Rename { take: t.id, name: name.trim().to_string() } });
                    }
                    let length = klipp_core::Timecode::from_secs_f64(secs, fps);
                    if ui.selectable_label(active, format!("{}  {}", if active { "\u{25CF} In the slot" } else { "Put in" }, state.timecode_display().duration(length))).on_hover_text(if active { "The take on the timeline" } else { "Put this take in the slot: one undo step" }).clicked() && !active {
                        cmds.push(AppCommand::EditAudition { clip_id, edit: AuditionEdit::Switch { take: t.id, ripple } });
                    }
                    if ui.add_enabled(!active, egui::Button::new("Remove")).on_disabled_hover_text("Put another take in first").clicked() {
                        cmds.push(AppCommand::EditAudition { clip_id, edit: AuditionEdit::Remove { take: t.id } });
                    }
                    ui.end_row();
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
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
        let mut frame = |state: &mut crate::state::AppState, events: Vec<egui::Event>| {
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
        let mut click = |state: &mut crate::state::AppState, label: &str| {
            // A window is laid out unseen in its first frame.
            frame(state, Vec::new());
            let (_, texts) = frame(state, Vec::new());
            let at = texts.iter().find(|(t, _)| t.starts_with(label)).map(|(_, p)| *p).unwrap_or_else(|| panic!("no {label:?}: {texts:?}"));
            let mut cmds = Vec::new();
            for pressed in [true, false] {
                cmds.extend(frame(state, vec![egui::Event::PointerMoved(at), egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]).0);
            }
            cmds
        };
        let cmds = click(&mut state, "Put in");
        assert!(matches!(&cmds[..], [AppCommand::EditAudition { clip_id, edit: AuditionEdit::Switch { take, ripple: true } }] if *clip_id == id && *take == second), "{cmds:?}");
        assert!(click(&mut state, "\u{25CF} In the slot").is_empty(), "the take in the slot is not switched to");
        let cmds = click(&mut state, "Add Copy");
        assert!(matches!(&cmds[..], [AppCommand::EditAudition { edit: AuditionEdit::AddCopy, .. }]), "{cmds:?}");
        let cmds = click(&mut state, "Finalize");
        assert!(matches!(&cmds[..], [AppCommand::EditAudition { edit: AuditionEdit::Finalize, .. }, AppCommand::OpenAuditionPicker { clip_id: None }]), "{cmds:?}");
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
