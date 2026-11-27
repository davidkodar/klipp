//! Slip, slide and roll as edits, with no timeline panel: a pointer
//! distance in, the edit to show and the step to make out.

use super::{RollGesture, SlideGesture, SlipGesture};
use crate::timeline::snap::Snap;
use klipp_core::{ClipId, Fps, RationalTime, SnapTargetKind};
use klipp_document::apply::DocumentView;
use klipp_document::history::EditCommand;
use klipp_document::live::LiveEdit;
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, Sequence, Track, TrackKind};

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 25)
}

fn free(_: &[RationalTime], _: &[ClipId], _: &mut Option<crate::timeline::snap::SnapHold>) -> Option<Snap> {
    None
}

/// A 30 s file; on V1 three 4 s clips of it end to end from 0 s, each 5 s
/// into the file; on A1 the middle one's sound, linked, a J cut: it starts
/// 1 s earlier on the timeline and in the file.
fn three_and_a_j_cut() -> (Sequence, MediaPool, [ClipId; 4]) {
    let mut pool = MediaPool::new();
    let mut media = MediaItem::synthetic_for_test("/tmp/cut-gestures.mov".into(), None);
    media.duration = tc(30.0);
    let m = pool.add(media);
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(25, 1);
    let mut v = Track::new("V1", TrackKind::Video);
    let mut a = Track::new("A1", TrackKind::Audio);
    v.clips = (0..3).map(|i| Clip::media(m, tc(4.0 * i as f64), tc(4.0 * i as f64 + 4.0), tc(5.0), tc(9.0))).collect();
    let mut sound = Clip::media(m, tc(3.0), tc(8.0), tc(4.0), tc(9.0));
    sound.linked_clip_id = Some(v.clips[1].id);
    v.clips[1].linked_clip_id = Some(sound.id);
    a.clips.push(sound);
    let ids = [v.clips[0].id, v.clips[1].id, v.clips[2].id, a.clips[0].id];
    seq.tracks = vec![v, a];
    (seq, pool, ids)
}

fn made(seq: &Sequence, pool: &MediaPool, edit: &EditCommand) -> Sequence {
    let (mut seqs, mut pool, mut fps) = (vec![seq.clone()], pool.clone(), seq.fps);
    DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_forward(edit);
    seqs.remove(0)
}

fn clip(seq: &Sequence, id: ClipId) -> Clip {
    seq.all_clips().find(|c| c.id == id).unwrap().clone()
}

/// `frame` shown 200 times, each in place of the last, then cancelled:
/// the sequence exactly as it began.
fn cancels_exactly(seq: &Sequence, pool: &MediaPool, mut frame: impl FnMut(usize) -> Option<EditCommand>) {
    let before = serde_json::to_value(seq).unwrap();
    let (mut seqs, mut pool, mut fps) = (vec![seq.clone()], pool.clone(), seq.fps);
    let mut live = LiveEdit::default();
    for k in 0..200 {
        if let Some(edit) = frame(k) {
            live.show(&mut DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps }, edit);
        }
    }
    assert!(live.is_live());
    live.cancel(&mut DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps });
    assert_eq!(serde_json::to_value(&seqs[0]).unwrap(), before, "given back");
}

fn swing(k: usize, by: f64) -> f64 {
    (k as f64 * 0.37).sin() * by
}

/// A slip moves the linked sound by as much as the picture, through its
/// own range: the J cut keeps its second (#458). The drag used to copy the
/// picture's range onto the sound, and putting that back for the step
/// left the sound a second off.
#[test]
fn a_slip_moves_a_j_cut_partner_through_its_own_range() {
    let (seq, pool, [_, mid, _, sound]) = three_and_a_j_cut();
    let mut g = SlipGesture::begin(&seq, &pool, 0, mid, true).unwrap();
    let shown = made(&seq, &pool, &g.drag_to(1.5).unwrap());
    assert_eq!((clip(&shown, mid).source_in, clip(&shown, mid).source_out), (tc(6.5), tc(10.5)));
    assert_eq!((clip(&shown, sound).source_in, clip(&shown, sound).source_out), (tc(5.5), tc(10.5)), "a second ahead, as it was");
    assert_eq!(clip(&shown, sound).timeline_in, tc(3.0), "a slip leaves it in its place");
    let step = g.release().unwrap();
    assert_eq!(serde_json::to_value(made(&seq, &pool, &step)).unwrap(), serde_json::to_value(&shown).unwrap(), "the step is what was shown");
    cancels_exactly(&seq, &pool, |k| g.drag_to(swing(k, 3.0)));
}

/// With Linked off at the press, the clip slips alone, shown and made.
/// The drag showed it alone and the step moved the sound too.
#[test]
fn unlinked_a_slip_moves_the_clip_alone() {
    let (seq, pool, [_, mid, _, sound]) = three_and_a_j_cut();
    let mut g = SlipGesture::begin(&seq, &pool, 0, mid, false).unwrap();
    let _ = g.drag_to(-2.0);
    let step = made(&seq, &pool, &g.release().unwrap());
    assert_eq!(clip(&step, mid).source_in, tc(3.0));
    assert_eq!(serde_json::to_value(clip(&step, sound)).unwrap(), serde_json::to_value(clip(&seq, sound)).unwrap(), "the sound where it was");
}

/// A slip stops at the file's start and end, and a slip back to where it
/// began is no step.
#[test]
fn a_slip_stops_at_the_file_and_back_where_it_began_is_no_step() {
    let (seq, pool, [_, mid, ..]) = three_and_a_j_cut();
    let mut g = SlipGesture::begin(&seq, &pool, 0, mid, true).unwrap();
    let at_start = made(&seq, &pool, &g.drag_to(-9.0).unwrap());
    assert_eq!((clip(&at_start, mid).source_in, clip(&at_start, mid).source_out), (tc(0.0), tc(4.0)));
    let at_end = made(&seq, &pool, &g.drag_to(40.0).unwrap());
    assert_eq!((clip(&at_end, mid).source_in, clip(&at_end, mid).source_out), (tc(26.0), tc(30.0)));
    assert!(g.drag_to(0.0).is_none() && g.release().is_none());
}

/// A slide moves the clip, its neighbours' facing edges and its linked
/// sound together, and stops with each neighbour a frame long.
#[test]
fn a_slide_moves_its_neighbours_and_partner_and_keeps_each_a_frame() {
    let (seq, pool, [left, mid, right, sound]) = three_and_a_j_cut();
    let mut g = SlideGesture::begin(&seq, 0, mid, true).unwrap();
    assert_eq!(g.movers(), vec![mid, left, right]);
    let shown = made(&seq, &pool, &g.drag_to(1.0, free).live.unwrap());
    assert_eq!((clip(&shown, mid).timeline_in, clip(&shown, mid).timeline_out), (tc(5.0), tc(9.0)));
    assert_eq!((clip(&shown, left).timeline_out, clip(&shown, left).source_out), (tc(5.0), tc(10.0)));
    assert_eq!((clip(&shown, right).timeline_in, clip(&shown, right).source_in), (tc(9.0), tc(6.0)));
    assert_eq!((clip(&shown, sound).timeline_in, clip(&shown, sound).timeline_out), (tc(4.0), tc(9.0)));
    assert_eq!(serde_json::to_value(made(&seq, &pool, &g.release().unwrap())).unwrap(), serde_json::to_value(&shown).unwrap(), "the step is what was shown");
    let far = made(&seq, &pool, &g.drag_to(10.0, free).live.unwrap());
    assert_eq!(clip(&far, right).timeline_out - clip(&far, right).timeline_in, RationalTime { value: 1, rate: 25 }, "the right neighbour a frame long");
    assert_eq!(g.right_moved(), Some((right, 3.96)));
    cancels_exactly(&seq, &pool, |k| g.drag_to(swing(k, 3.0), free).live);
}

/// A slide snaps its clip's edge, its moving neighbours left out, and draws
/// the line only where the slide reached the snap.
#[test]
fn a_slide_snaps_and_draws_the_line_where_it_got_there() {
    let (seq, _, [left, mid, right, _]) = three_and_a_j_cut();
    let mut g = SlideGesture::begin(&seq, 0, mid, true).unwrap();
    let to = |at: f64| move |edges: &[RationalTime], movers: &[ClipId], _: &mut _| {
        assert_eq!(movers, [mid, left, right]);
        Some(Snap { edge: 1, at: edges[1] + tc(at), kind: SnapTargetKind::Playhead, owner: None })
    };
    let f = g.drag_to(0.0, to(0.2));
    assert_eq!((g.by, f.snapped.map(|s| s.at)), (Some(tc(0.2)), Some(tc(8.2))));
    let f = g.drag_to(3.0, to(2.0));
    assert!(f.snapped.is_none(), "held short of the snap: no line");
}

/// A roll moves the cut, a's end and b's start together, each linked
/// partner by the same amounts from its own times; it stops with b a frame
/// long. What it sends is made by the app's roll as it was shown.
#[test]
fn a_roll_moves_the_cut_and_the_partners_from_their_own_times() {
    let (seq, pool, [left, mid, _, sound]) = three_and_a_j_cut();
    let mut g = RollGesture::begin(&seq, &pool, 0, left, mid, true).unwrap();
    let f = g.drag_to(tc(0.5), free);
    assert_eq!((f.cut, f.a_length), (tc(4.5), tc(4.5)));
    let shown = made(&seq, &pool, f.live.as_ref().unwrap());
    assert_eq!((clip(&shown, left).timeline_out, clip(&shown, left).source_out), (tc(4.5), tc(9.5)));
    assert_eq!((clip(&shown, mid).timeline_in, clip(&shown, mid).source_in), (tc(4.5), tc(5.5)));
    assert_eq!((clip(&shown, sound).timeline_in, clip(&shown, sound).source_in), (tc(3.5), tc(4.5)), "the J cut's sound from its own start");

    let mut state = crate::state::AppState::default();
    state.document.sequences = vec![seq.clone()];
    state.document.media_pool = pool.clone();
    let Some(klipp_core::AppCommand::RollTrimClip { clip_a_id, clip_b_id, new_a_source_out, new_a_timeline_out, new_b_source_in, new_b_timeline_in, linked }) = g.release() else { panic!("a roll sent") };
    state.roll_trim(clip_a_id, clip_b_id, (new_a_source_out, new_a_timeline_out), (new_b_source_in, new_b_timeline_in), linked);
    assert_eq!(serde_json::to_value(state.sequence()).unwrap(), serde_json::to_value(&shown).unwrap(), "made as it was shown");

    let far = g.drag_to(tc(9.0), free);
    assert_eq!(far.cut, tc(8.0) - RationalTime { value: 1, rate: 25 }, "b a frame long");
    let _ = g.drag_to(tc(0.0), free);
    assert!(g.release().is_none(), "the cut where it was: no step");
    cancels_exactly(&seq, &pool, |k| g.drag_to(tc(swing(k, 2.0)), free).live);
}

/// Unlinked, a roll leaves the partners where they are.
#[test]
fn unlinked_a_roll_leaves_the_partners() {
    let (seq, pool, [left, mid, _, sound]) = three_and_a_j_cut();
    let mut g = RollGesture::begin(&seq, &pool, 0, left, mid, false).unwrap();
    let shown = made(&seq, &pool, g.drag_to(tc(-1.0), free).live.as_ref().unwrap());
    assert_eq!(clip(&shown, mid).timeline_in, tc(3.0));
    assert_eq!(serde_json::to_value(clip(&shown, sound)).unwrap(), serde_json::to_value(clip(&seq, sound)).unwrap());
}
