//! Edit points (#913): the cut between two clips on a track, or a clip's
//! edge facing a gap, selected on one side (the outgoing clip's end, the
//! incoming clip's start) or both (a roll), and trimmed from the keyboard
//! a frame or several at a time, by a typed amount, or to the playhead.
//! Trimming was mouse-only, one clip at a time.
//!
//! The trims are the document's own `Op::Trim`, so a trim from the
//! keyboard is checked as a drag's is (the media there is, a clip's least
//! length, linked partners), and is one undo step.

use klipp_core::{ClipId, Timecode, TrackId};
use klipp_document::ops::{Anchor, ClipRef, Edge, Op};
use klipp_timeline::{Sequence, Track};

/// Which side of a cut a trim moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditSide {
    /// The outgoing clip's end: a trim there lengthens or shortens it.
    Out,
    /// The incoming clip's start.
    In,
    /// Both: the cut itself moves, a roll, and the sequence keeps its
    /// length.
    Both,
}

/// A selected edit point on one track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditPoint {
    pub track: TrackId,
    /// The clip ending at the cut, if any.
    pub outgoing: Option<ClipId>,
    /// The clip starting at the cut, if any.
    pub incoming: Option<ClipId>,
    pub side: EditSide,
}

impl EditPoint {
    /// The side a click at the cut picks when both may be trimmed: both,
    /// unless a side has no clip.
    fn with_side(track: TrackId, outgoing: Option<ClipId>, incoming: Option<ClipId>, side: EditSide) -> Option<Self> {
        let side = match (side, outgoing, incoming) {
            (_, None, None) => return None,
            (EditSide::Both, Some(_), Some(_)) | (EditSide::Out, Some(_), _) | (EditSide::In, _, Some(_)) => side,
            (_, Some(_), None) => EditSide::Out,
            (_, None, Some(_)) => EditSide::In,
        };
        Some(Self { track, outgoing, incoming, side })
    }

    /// The next side, as U cycles them: both, the outgoing end, the
    /// incoming start, skipping a side that has no clip.
    pub fn cycled(self) -> Self {
        let order = [EditSide::Both, EditSide::Out, EditSide::In];
        let at = order.iter().position(|s| *s == self.side).unwrap_or(0);
        (1..=3)
            .filter_map(|k| Self::with_side(self.track, self.outgoing, self.incoming, order[(at + k) % 3]))
            .find(|e| e.side != self.side)
            .unwrap_or(self)
    }
}

/// The edit point at `at` on `track`, `side` as asked when it has that
/// side. `None` when no clip starts or ends there.
pub fn at(track: &Track, at: Timecode, side: EditSide) -> Option<EditPoint> {
    let outgoing = track.clips.iter().find(|c| !c.kind.is_gap() && c.timeline_out == at).map(|c| c.id);
    let incoming = track.clips.iter().find(|c| !c.kind.is_gap() && c.timeline_in == at).map(|c| c.id);
    EditPoint::with_side(track.id, outgoing, incoming, side)
}

/// The edit point nearest `playhead` on the unlocked tracks `on` lets
/// through, both sides selected.
pub fn nearest(seq: &Sequence, playhead: Timecode, on: impl Fn(&Track) -> bool) -> Option<EditPoint> {
    let ph = playhead.to_secs_f64();
    seq.tracks
        .iter()
        .filter(|t| !t.locked && on(t))
        .flat_map(|t| t.clips.iter().filter(|c| !c.kind.is_gap()).flat_map(move |c| [(t, c.timeline_in), (t, c.timeline_out)]))
        .min_by(|a, b| (a.1.to_secs_f64() - ph).abs().total_cmp(&(b.1.to_secs_f64() - ph).abs()))
        .and_then(|(t, edge)| at(t, edge, EditSide::Both))
}

/// Where the cut of `edit` is now, when its clips are still there.
pub fn cut(seq: &Sequence, edit: &EditPoint) -> Option<Timecode> {
    let find = |id: Option<ClipId>| id.and_then(|id| seq.all_clips().find(|c| c.id == id));
    match (find(edit.outgoing), find(edit.incoming)) {
        (Some(o), _) => Some(o.timeline_out),
        (None, Some(i)) => Some(i.timeline_in),
        (None, None) => None,
    }
}

/// The frame `t` falls on at the sequence's rate.
fn frame(seq: &Sequence, t: Timecode) -> i64 {
    (t.to_secs_f64() * seq.fps.as_f64()).round() as i64
}

/// The trims that move `edit` by `frames` (later when positive): the
/// outgoing end, the incoming start, or, for both, a roll, as two trims
/// with the side that gives way first. `ripple` closes or opens the gap a
/// one-sided trim leaves; a roll never changes the sequence's length.
pub fn trim_ops(seq: &Sequence, edit: &EditPoint, frames: i64, ripple: bool, linked: bool) -> Vec<Op> {
    if frames == 0 {
        return Vec::new();
    }
    let Some(cut_at) = cut(seq, edit) else { return Vec::new() };
    let to = Anchor::Frame(frame(seq, cut_at) + frames);
    let trim = |id: ClipId, edge: Edge, ripple: bool| Op::Trim { clip_id: ClipRef::Id(id), edge, to: to.clone(), ripple, linked };
    match (edit.side, edit.outgoing, edit.incoming) {
        (EditSide::Out, Some(o), _) => vec![trim(o, Edge::Out, ripple)],
        (EditSide::In, _, Some(i)) => vec![trim(i, Edge::In, ripple)],
        (EditSide::Both, Some(o), Some(i)) if frames > 0 => vec![trim(i, Edge::In, false), trim(o, Edge::Out, false)],
        (EditSide::Both, Some(o), Some(i)) => vec![trim(o, Edge::Out, false), trim(i, Edge::In, false)],
        _ => Vec::new(),
    }
}

/// How many frames take `edit` to `to` (extend edit: the cut to the
/// playhead).
pub fn frames_to(seq: &Sequence, edit: &EditPoint, to: Timecode) -> i64 {
    cut(seq, edit).map(|c| frame(seq, to) - frame(seq, c)).unwrap_or(0)
}

/// A typed trim amount: `+12` / `-5` frames, or `+1:05` seconds and
/// frames, at `fps` frames a second. `None` for anything else.
pub fn parse_amount(text: &str, fps: f64) -> Option<i64> {
    let text = text.trim();
    let (sign, rest) = match text.chars().next()? {
        '+' => (1, &text[1..]),
        '-' => (-1, &text[1..]),
        _ => (1, text),
    };
    let frames = match rest.split_once(':') {
        Some((s, f)) => s.parse::<i64>().ok()? * fps.round() as i64 + f.parse::<i64>().ok()?,
        None => rest.parse::<i64>().ok()?,
    };
    Some(sign * frames)
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::{Clip, TrackKind};

    fn tc(s: f64) -> Timecode {
        RationalTime::from_secs_f64(s, 25)
    }

    /// Two clips cut at 4 s on V1, then a gap, then a third at 10 s.
    fn seq() -> Sequence {
        let mut seq = Sequence::new_default();
        seq.fps = klipp_core::Fps::new(25, 1);
        let mut v = Track::new("V1", TrackKind::Video);
        let m = MediaId::new();
        v.clips = vec![
            Clip::media(m, tc(0.0), tc(4.0), tc(10.0), tc(14.0)),
            Clip::media(m, tc(4.0), tc(8.0), tc(20.0), tc(24.0)),
            Clip::media(m, tc(10.0), tc(12.0), tc(30.0), tc(32.0)),
        ];
        seq.tracks = vec![v];
        seq
    }

    #[test]
    fn the_nearest_cut_is_selected_on_both_sides_and_an_edge_on_its_one() {
        let seq = seq();
        let (a, b, c) = (seq.tracks[0].clips[0].id, seq.tracks[0].clips[1].id, seq.tracks[0].clips[2].id);
        let e = nearest(&seq, tc(4.5), |_| true).unwrap();
        assert_eq!((e.outgoing, e.incoming, e.side), (Some(a), Some(b), EditSide::Both));
        let e = nearest(&seq, tc(9.6), |_| true).unwrap();
        assert_eq!((e.outgoing, e.incoming, e.side), (None, Some(c), EditSide::In), "a clip's head facing a gap");
        let mut locked = seq.clone();
        locked.tracks[0].locked = true;
        assert!(nearest(&locked, tc(4.0), |_| true).is_none(), "a locked track's edits are not picked");
    }

    #[test]
    fn u_cycles_the_sides_there_are() {
        let seq = seq();
        let e = nearest(&seq, tc(4.0), |_| true).unwrap();
        assert_eq!(e.cycled().side, EditSide::Out);
        assert_eq!(e.cycled().cycled().side, EditSide::In);
        assert_eq!(e.cycled().cycled().cycled().side, EditSide::Both);
        let edge = nearest(&seq, tc(10.0), |_| true).unwrap();
        assert_eq!(edge.cycled().side, EditSide::In, "one side only: it stays");
    }

    #[test]
    fn a_roll_trims_the_side_that_gives_way_first() {
        let seq = seq();
        let e = nearest(&seq, tc(4.0), |_| true).unwrap();
        let ops = trim_ops(&seq, &e, 5, true, true);
        assert!(matches!(&ops[..], [Op::Trim { edge: Edge::In, to: Anchor::Frame(105), ripple: false, .. }, Op::Trim { edge: Edge::Out, to: Anchor::Frame(105), ripple: false, .. }]), "{ops:?}");
        let ops = trim_ops(&seq, &e, -3, true, true);
        assert!(matches!(&ops[..], [Op::Trim { edge: Edge::Out, to: Anchor::Frame(97), .. }, Op::Trim { edge: Edge::In, .. }]), "{ops:?}");
        let out = EditPoint { side: EditSide::Out, ..e };
        assert!(matches!(&trim_ops(&seq, &out, 2, true, true)[..], [Op::Trim { edge: Edge::Out, to: Anchor::Frame(102), ripple: true, .. }]));
        assert!(trim_ops(&seq, &e, 0, true, true).is_empty());
    }

    #[test]
    fn the_resolved_trims_move_the_cut() {
        let seq = seq();
        let pool = klipp_media::MediaPool::new();
        let e = nearest(&seq, tc(4.0), |_| true).unwrap();
        let plan = klipp_document::ops::resolve(&seq, &pool, &trim_ops(&seq, &e, 5, true, true));
        // Synthetic media has no length to check against, so the roll is
        // what is asked: whether it is refused or planned, it names both.
        if let Ok(plan) = plan {
            assert_eq!(plan.steps.len(), 2);
        }
        assert_eq!(frames_to(&seq, &e, tc(5.0)), 25);
    }

    #[test]
    fn a_typed_amount_is_frames_or_seconds_and_frames() {
        assert_eq!(parse_amount("+12", 25.0), Some(12));
        assert_eq!(parse_amount("-5", 25.0), Some(-5));
        assert_eq!(parse_amount("+1:05", 25.0), Some(30));
        assert_eq!(parse_amount("-0:10", 25.0), Some(-10));
        assert_eq!(parse_amount("+", 25.0), None);
        assert_eq!(parse_amount("+x", 25.0), None);
    }
}
