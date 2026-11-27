//! The marks after an edit that shortens the timeline: Out brought in to
//! the new end in the edit's own step, as every committed step's
//! follow-up does.

use crate::state::AppState;

/// A timeline of two clips, 0-10 s and 10-20 s, marked 8 to 18 s, as
/// the frame before an edit sees it.
fn marked_state() -> AppState {
    let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut s = AppState::default();
    let p = s.sequence().primary_track_index().unwrap();
    let idx = s.active_sequence_idx;
    s.document.sequences[idx].tracks[p].clips = vec![
        klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(10.0), t(0.0), t(10.0)),
        klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(10.0), t(20.0), t(0.0), t(10.0)),
    ];
    s.document.sequences[idx].in_point = Some(t(8.0));
    s.document.sequences[idx].out_point = Some(t(18.0));
    s.note_timeline_length();
    s
}

/// The tracks cut to end at `secs`, committed as a handler's edit is:
/// no step when they already end there.
fn cut_to(s: &mut AppState, secs: f64) {
    let end = klipp_core::RationalTime::from_secs_f64(secs, 24);
    let p = s.sequence().primary_track_index().unwrap();
    let _ = s.commit_tracks("Close Gap", |seq| {
        let clips = &mut seq.tracks[p].clips;
        clips.retain(|c| c.timeline_in < end);
        if let Some(last) = clips.last_mut() {
            last.timeline_out = end;
        }
    });
}

fn marks(s: &AppState) -> (Option<f64>, Option<f64>) {
    (s.sequence().in_point.map(|t| t.to_secs_f64()), s.sequence().out_point.map(|t| t.to_secs_f64()))
}

fn apply(s: &mut AppState, undo: bool) {
    let entry = if undo { s.take_undo() } else { s.take_redo() }.expect("a step");
    let active = s.active_sequence_idx;
    let mut view = klipp_document::apply::DocumentView { sequences: &mut s.document.sequences, active, media_pool: &mut s.document.media_pool, fps: &mut s.fps };
    if undo {
        view.apply_inverse(&entry.command);
    } else {
        view.apply_forward(&entry.command);
    }
}

/// An edit that shortens the timeline brings Out in to the new end, in
/// its own undo step: undone, the marks and the clips are back together.
/// Out stayed past the end, and the export defaulted to that range,
/// black and silent after the last clip.
#[test]
fn an_edit_that_shortens_the_timeline_brings_out_to_the_new_end() {
    let mut s = marked_state();
    let steps = s.active_history().undo_count();
    cut_to(&mut s, 12.0);
    assert_eq!(marks(&s), (Some(8.0), Some(12.0)));
    assert_eq!(s.active_history().undo_count(), steps + 1, "one step");
    assert_eq!(s.undo_label().as_deref(), Some("Close Gap"), "under the edit's name");
    apply(&mut s, true);
    assert_eq!(marks(&s), (Some(8.0), Some(18.0)), "undone");
    assert_eq!(s.sequence().total_duration().to_secs_f64(), 20.0);
    apply(&mut s, false);
    assert_eq!(marks(&s), (Some(8.0), Some(12.0)), "redone");
    assert_eq!(s.sequence().total_duration().to_secs_f64(), 12.0);
}

#[test]
fn marks_wholly_past_the_new_end_are_cleared() {
    let mut s = marked_state();
    cut_to(&mut s, 5.0);
    assert_eq!(marks(&s), (None, None));
    apply(&mut s, true);
    assert_eq!(marks(&s), (Some(8.0), Some(18.0)), "undone");
}

/// Past the end with nothing shortened, they are someone's: a range
/// marked for a three-point edit or a fit to fill, and an edit that
/// leaves the timeline's length alone or adds to it doesn't touch it.
#[test]
fn marks_past_an_end_nothing_shortened_stay() {
    let mut s = marked_state();
    let idx = s.active_sequence_idx;
    s.document.sequences[idx].out_point = Some(klipp_core::RationalTime::from_secs_f64(30.0, 24));
    cut_to(&mut s, 20.0);
    s.mark_range(Some(Some(klipp_core::RationalTime::from_secs_f64(25.0, 24))), None);
    assert_eq!(marks(&s), (Some(25.0), Some(30.0)), "marked past the end on purpose");
}

/// Undone, then cut again in a later frame: the second cut is still a
/// shortening, as the frame noted the longer timeline.
#[test]
fn a_cut_after_an_undo_brings_the_marks_in_too() {
    let mut s = marked_state();
    cut_to(&mut s, 12.0);
    apply(&mut s, true);
    s.note_timeline_length();
    cut_to(&mut s, 12.0);
    assert_eq!(marks(&s), (Some(8.0), Some(12.0)));
}
