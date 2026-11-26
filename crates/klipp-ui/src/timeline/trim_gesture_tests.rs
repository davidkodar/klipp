//! The edge drag as an edit, with no timeline panel: a pointer distance
//! in, the trim to show and the trim to send out.

use super::{trim_limits, TrimGesture};
use klipp_core::AppCommand;
use crate::timeline::snap::Snap;
use klipp_core::{Fps, RationalTime, SnapTargetKind};
use klipp_document::apply::DocumentView;
use klipp_document::history::EditCommand;
use klipp_document::ops::Edge;
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, Sequence, Track, TrackKind};

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 25)
}

/// No snapping.
fn free(_: RationalTime, _: klipp_core::ClipId, _: &mut Option<crate::timeline::snap::SnapHold>) -> Option<Snap> {
    None
}

/// A 20 s file and, on V1 and A1, two 4 s clips of it end to end, each
/// picture linked to its sound; A1 locked if `locked`.
fn two_linked(locked: bool) -> (Sequence, MediaPool) {
    let mut pool = MediaPool::new();
    let mut media = MediaItem::synthetic_for_test("/tmp/trim-gesture.mov".into(), None);
    media.duration = tc(20.0);
    let media_id = pool.add(media);
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(25, 1);
    let mut v = Track::new("V1", TrackKind::Video);
    let mut a = Track::new("A1", TrackKind::Audio);
    for i in 0..2 {
        let start = i as f64 * 4.0;
        let mut video = Clip::media(media_id, tc(start), tc(start + 4.0), tc(start), tc(start + 4.0));
        let mut audio = video.clone();
        audio.id = klipp_core::ClipId::new();
        video.linked_clip_id = Some(audio.id);
        audio.linked_clip_id = Some(video.id);
        v.clips.push(video);
        a.clips.push(audio);
    }
    a.locked = locked;
    seq.tracks = vec![v, a];
    (seq, pool)
}

/// `edit` made on `seq`.
fn made(seq: &Sequence, pool: &MediaPool, edit: &EditCommand) -> Sequence {
    let (mut seqs, mut pool, mut fps) = (vec![seq.clone()], pool.clone(), seq.fps);
    DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_forward(edit);
    seqs.remove(0)
}

/// A ripple trim of a linked picture's end shows both halves trimmed and
/// the clips after them moved once; what it sends plans the same trim on
/// the timeline as the drag found it. A locked partner refuses: nothing is
/// shown and nothing sent.
#[test]
fn a_linked_ripple_trim_shows_what_it_sends_and_a_locked_partner_refuses() {
    for locked in [false, true] {
        let (seq, pool) = two_linked(locked);
        let id = seq.tracks[0].clips[0].id;
        let mut g = TrimGesture::begin(&seq, 0, id, Edge::Out, true, true).unwrap();
        let frame = g.drag_to(tc(-1.0), seq.fps, &pool, free);
        if locked {
            assert!(frame.live.is_none() && g.target.is_none(), "refused: nothing shown");
            assert!(g.release().is_none(), "and nothing sent");
            continue;
        }
        let shown = made(&seq, &pool, frame.live.as_ref().unwrap());
        for track in &shown.tracks {
            assert_eq!(track.clips[0].timeline_out, tc(3.0));
            assert_eq!(track.clips[1].timeline_in, tc(3.0), "moved once, never twice");
        }
        let sent = g.release().unwrap();
        let planned = crate::clip_edits::trim(&seq, &pool, &sent).unwrap().unwrap();
        assert_eq!(serde_json::to_value(made(&seq, &pool, &planned)).unwrap(), serde_json::to_value(&shown).unwrap(), "what lands is what was shown");
    }
}

/// The last clip's head trimmed with ripple on keeps the ripple, though no
/// clip follows it.
#[test]
fn the_last_clip_keeps_the_drag_ripple_mode_without_following_clips() {
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(25, 1);
    let mut track = Track::new("V1", TrackKind::Video);
    let c = Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0));
    track.clips.push(c.clone());
    seq.tracks = vec![track];
    let mut g = TrimGesture::begin(&seq, 0, c.id, Edge::In, true, true).unwrap();
    let _ = g.drag_to(tc(1.0), seq.fps, &MediaPool::new(), free);
    assert!(matches!(g.release(), Some(AppCommand::TrimClip { ripple: true, .. })));
}

/// #3055: a trim reads the file through the clip's speed and direction. A
/// clip at 2x of a 20 s file from 2 s to 10 s can have its end trimmed on
/// by 5 s of timeline (the file's last 10 s) and its start back by 1 s;
/// reversed, the other way round. Past the file's end the edge stops there
/// and a stretch is offered where the pointer went.
#[test]
fn a_trim_reads_the_file_through_the_clips_speed() {
    let mut pool = MediaPool::new();
    let mut media = MediaItem::synthetic_for_test("/tmp/trim-speed.mov".into(), None);
    media.duration = tc(20.0);
    let media_id = pool.add(media);
    let mut c = Clip::media(media_id, tc(10.0), tc(14.0), tc(2.0), tc(10.0));
    c.speed = 2.0;
    assert_eq!(trim_limits(&c, &pool), (9.0, 19.0));
    let mut r = c.clone();
    r.speed = -2.0;
    assert_eq!(trim_limits(&r, &pool), (5.0, 15.0), "reversed: its head is the file's end");
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(25, 1);
    let mut v = Track::new("V1", TrackKind::Video);
    v.clips = vec![c.clone()];
    seq.tracks = vec![v];
    let mut g = TrimGesture::begin(&seq, 0, c.id, Edge::Out, false, true).unwrap();
    let frame = g.drag_to(tc(1.0), seq.fps, &pool, free);
    let after = made(&seq, &pool, frame.live.as_ref().unwrap());
    let after = &after.tracks[0].clips[0];
    assert_eq!((after.timeline_out, after.source_out), (tc(15.0), tc(12.0)), "1 s of timeline is 2 s of the file");
    assert!(!g.at_media_end);
    let frame = g.drag_to(tc(8.0), seq.fps, &pool, free);
    let after = made(&seq, &pool, frame.live.as_ref().unwrap());
    assert_eq!(after.tracks[0].clips[0].timeline_out, tc(19.0), "held at the file's end");
    assert!(g.at_media_end, "and a stretch offered");
    assert_eq!(g.wanted, Some(tc(22.0)), "where the pointer went");
}

/// The edge lands on a snap the snapper finds, and the line is drawn
/// there; the snap's hold is the gesture's, kept from frame to frame.
#[test]
fn the_edge_lands_on_a_snap() {
    let (seq, pool) = two_linked(false);
    let id = seq.tracks[0].clips[1].id;
    let mut g = TrimGesture::begin(&seq, 0, id, Edge::Out, false, false).unwrap();
    let mut asked = Vec::new();
    let frame = g.drag_to(tc(-0.4), seq.fps, &pool, |at, clip, _| {
        asked.push((at, clip));
        Some(Snap { edge: 0, at: tc(7.0), kind: SnapTargetKind::Playhead, owner: None })
    });
    assert_eq!(asked, [(tc(7.6), id)], "the dragged edge asked, its own clip left out");
    assert_eq!(frame.snapped.map(|s| s.at), Some(tc(7.0)));
    assert_eq!(g.target, Some(tc(7.0)));
}

/// 200 frames of a drag back and forth, each shown in place of the last,
/// then cancelled: the timeline as the drag found it.
#[test]
fn two_hundred_frames_then_a_cancel_give_the_timeline_back() {
    for (edge, ripple) in [(Edge::Out, true), (Edge::In, true), (Edge::Out, false), (Edge::In, false)] {
        let (seq, mut pool) = two_linked(false);
        let id = seq.tracks[0].clips[0].id;
        let before = serde_json::to_value(&seq).unwrap();
        let mut g = TrimGesture::begin(&seq, 0, id, edge, ripple, true).unwrap();
        let (mut seqs, mut fps) = (vec![seq.clone()], seq.fps);
        let mut live = klipp_document::live::LiveEdit::default();
        for k in 0..200 {
            let dt = tc((k as f64 * 0.37).sin() * 2.5);
            if let Some(edit) = g.drag_to(dt, fps, &pool, free).live {
                live.show(&mut DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps }, edit);
            }
        }
        assert!(live.is_live(), "{edge:?} ripple {ripple}: shown");
        live.cancel(&mut DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps });
        assert_eq!(serde_json::to_value(&seqs[0]).unwrap(), before, "{edge:?} ripple {ripple}: given back");
    }
}
