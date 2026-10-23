//! A multicam primary's cut times count from its start: the app's own
//! edits that make a piece of it or move the picture under its start (a
//! lifted range, a roll) leave every moment of the picture cut to the
//! angle it was, and undo puts the cuts back.

use klipp_core::{GroupId, MediaId, Timecode};
use klipp_document::apply::DocumentView;
use klipp_document::history::EditCommand;
use klipp_timeline::{Clip, Sequence, Track, TrackKind};

fn t(s: f64) -> Timecode {
    Timecode::from_secs_f64(s, 24)
}

/// Two angles, each a 0-4 s and a 4-8 s piece of one 8 s take; the
/// primary (track 0) cut to angle 1 at 1 s and 5 s and back at 3 s and 7
/// s of the take.
fn cut() -> Sequence {
    let (g, media) = (GroupId::new(), MediaId::new());
    let mut seq = Sequence::new_default();
    seq.tracks = (0..2u8)
        .map(|angle| {
            let clips = [(0.0, 4.0, vec![(1.0, 1), (3.0, 0)]), (4.0, 8.0, vec![(1.0, 1), (3.0, 0)])]
                .into_iter()
                .map(|(from, to, cuts)| {
                    let mut c = Clip::media(media, t(from), t(to), t(from), t(to));
                    (c.multicam_group, c.multicam_angle) = (Some(g), angle);
                    if angle == 0 {
                        c.multicam_cuts = cuts;
                    }
                    c
                })
                .collect();
            Track { clips, ..Track::new("V", TrackKind::Video) }
        })
        .collect();
    seq
}

/// The angle a moment of the take is cut to, through whichever piece of
/// the primary shows it: `None` where none does.
fn angle_of_picture(seq: &Sequence, s: f64) -> Option<u8> {
    seq.tracks[0].clips.iter().find(|c| s >= c.source_in.to_secs_f64() && s < c.source_out.to_secs_f64()).map(|c| {
        let rel = s - c.source_in.to_secs_f64();
        c.multicam_cuts.iter().rfind(|(at, _)| *at <= rel + 1e-9).map_or(0, |(_, a)| *a)
    })
}

fn same_picture(before: &Sequence, after: &Sequence, what: &str) {
    for s in (0..32).map(|k| k as f64 * 0.25 + 0.125) {
        if let Some(now) = angle_of_picture(after, s) {
            assert_eq!(Some(now), angle_of_picture(before, s), "{what}: the picture at {s} s of the take");
        }
    }
}

/// A lifted range through the middle of a piece and over its head.
#[test]
fn a_lifted_range_keeps_each_angle_on_its_picture() {
    let before = cut();
    for (from, to) in [(1.5, 2.5), (3.0, 5.5)] {
        let mut seq = before.clone();
        for track in &mut seq.tracks {
            super::KlippApp::lift_range(track, t(from), t(to));
        }
        same_picture(&before, &seq, &format!("lift {from}-{to} s"));
    }
}

/// A roll moves the join, and the incoming piece's start with it: as the
/// app's roll does, the roll and then the cuts kept, recorded together.
#[test]
fn a_roll_keeps_each_angle_on_its_picture_and_undoes() {
    let before = cut();
    let mut seq = before.clone();
    let (a, b) = (seq.tracks[0].clips[0].clone(), seq.tracks[0].clips[1].clone());
    let to = t(5.0);
    let roll = EditCommand::LegacyRollTrimClip {
        clip_a_id: a.id,
        clip_b_id: b.id,
        old_a_source_out: a.source_out,
        old_a_timeline_out: a.timeline_out,
        old_b_source_in: b.source_in,
        old_b_timeline_in: b.timeline_in,
        new_a_source_out: a.source_out + (to - a.timeline_out),
        new_a_timeline_out: to,
        new_b_source_in: b.source_in + (to - b.timeline_in),
        new_b_timeline_in: to,
    };
    let mut seqs = vec![seq.clone()];
    let mut pool = klipp_media::MediaPool::new();
    let mut fps = seq.fps;
    DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_forward(&roll);
    seq = seqs.remove(0);
    let cuts = klipp_document::ops::keep_angle_cuts(&mut seq, b.id, (to - b.timeline_in).to_secs_f64()).expect("the incoming piece's cuts move");
    same_picture(&before, &seq, "roll");
    let step = EditCommand::Batch(vec![roll, cuts]);
    let mut seqs = vec![seq];
    DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_inverse(&step);
    let lists = |s: &Sequence| s.tracks[0].clips.iter().map(|c| c.multicam_cuts.clone()).collect::<Vec<_>>();
    assert_eq!(lists(&seqs[0]), lists(&before), "undo puts the cuts back");
}
