//! What dragging a clip's body to slip or slide it, or a cut to roll it,
//! means as an edit (U2). No painting and no pointer: the timeline hands
//! over how far the pointer has gone, gets back the whole edit from where
//! the drag began, and the project shows it as its live edit in place of
//! the last frame's. What is shown is the step made at the end, partners
//! and all.
//!
//! A clip's linked partner follows only when the drag began with it
//! linked (#916); it moves through its own times, so a J or L cut keeps
//! its offset (#458).

use klipp_core::{ClipId, RationalTime, Timecode};
use klipp_document::history::EditCommand;
use klipp_media::MediaPool;
use klipp_timeline::{Clip, Sequence};

use crate::timeline::snap::{Snap, SnapHold};

/// Snaps the dragged edges given, leaving out the clips named, through the
/// hold it is given (`Snapper::snap`).
pub(crate) trait Snaps: FnMut(&[RationalTime], &[ClipId], &mut Option<SnapHold>) -> Option<Snap> {}
impl<F: FnMut(&[RationalTime], &[ClipId], &mut Option<SnapHold>) -> Option<Snap>> Snaps for F {}

fn find(seq: &Sequence, id: ClipId) -> Option<Clip> {
    seq.all_clips().find(|c| c.id == id).cloned()
}

/// `was` changed to `now`, as a step; none when nothing changed.
fn replaced(description: &'static str, was: &Clip, now: Clip) -> Option<EditCommand> {
    let same = was.source_in == now.source_in && was.source_out == now.source_out && was.timeline_in == now.timeline_in && was.timeline_out == now.timeline_out;
    (!same).then(|| EditCommand::ReplaceClip { description, old: Box::new(was.clone()), new: Box::new(now) })
}

/// The clips as the drag found them, each changed as `change` has it,
/// as one edit; `None` when none changed.
fn edit_of(description: &'static str, clips: impl IntoIterator<Item = (Clip, Clip)>) -> Option<EditCommand> {
    let edits: Vec<EditCommand> = clips.into_iter().filter_map(|(was, now)| replaced(description, &was, now)).collect();
    (!edits.is_empty()).then_some(EditCommand::Batch(edits))
}

/// A slip under way: the clip's source window moved under its place.
pub(crate) struct SlipGesture {
    pub clip_id: ClipId,
    pub track_idx: usize,
    was: Clip,
    partner: Option<Clip>,
    media_duration: Option<Timecode>,
    /// The source window at the last frame.
    pub window: Option<(Timecode, Timecode)>,
}

impl SlipGesture {
    pub fn begin(seq: &Sequence, pool: &MediaPool, track_idx: usize, clip_id: ClipId, linked: bool) -> Option<Self> {
        let was = seq.tracks.get(track_idx)?.find_clip(clip_id)?.clone();
        let partner = was.linked_clip_id.filter(|_| linked).and_then(|id| find(seq, id));
        let media_duration = was.media_id().and_then(|m| pool.get(&m)).map(|m| m.duration);
        Some(Self { clip_id, track_idx, was, partner, media_duration, window: None })
    }

    /// The pointer `secs` along: the slip to show, held inside the file.
    pub fn drag_to(&mut self, secs: f64) -> Option<EditCommand> {
        let dt = RationalTime::from_secs_f64(secs, self.was.source_in.rate);
        let window = crate::panels::timeline::slip_clamped_source_range(self.was.source_in, self.was.source_out, dt, self.media_duration);
        self.window = Some(window);
        self.edit(window)
    }

    /// How far the window's start moved at the last frame.
    pub fn by(&self) -> Option<Timecode> {
        self.window.map(|(si, _)| si - self.was.source_in)
    }

    fn edit(&self, (si, so): (Timecode, Timecode)) -> Option<EditCommand> {
        let by = si - self.was.source_in;
        let mut now = self.was.clone();
        (now.source_in, now.source_out) = (si, so);
        let partner = self.partner.clone().map(|p| {
            let mut moved = p.clone();
            moved.source_in += by;
            moved.source_out += by;
            (p, moved)
        });
        edit_of("Slip", std::iter::once((self.was.clone(), now)).chain(partner))
    }

    /// The slip to record: the last frame's, none when it moved nothing.
    pub fn release(&self) -> Option<EditCommand> {
        self.edit(self.window?).map(|e| EditCommand::Named { label: "Slip".into(), edit: Box::new(e) })
    }

    pub fn was(&self) -> &Clip {
        &self.was
    }
}

/// A slide under way: the clip moved between its neighbours, the left's
/// end and the right's start following it.
pub(crate) struct SlideGesture {
    pub clip_id: ClipId,
    pub track_idx: usize,
    was: Clip,
    left: Option<Clip>,
    right: Option<Clip>,
    partner: Option<Clip>,
    hold: Option<SnapHold>,
    /// How far the clip moved at the last frame.
    pub by: Option<Timecode>,
}

/// One frame of a slide: the edit to show, and the snap to draw a line at.
#[derive(Debug, Default)]
pub(crate) struct SlideFrame {
    pub live: Option<EditCommand>,
    pub snapped: Option<Snap>,
}

impl SlideGesture {
    pub fn begin(seq: &Sequence, track_idx: usize, clip_id: ClipId, linked: bool) -> Option<Self> {
        let clips = &seq.tracks.get(track_idx)?.clips;
        let i = clips.iter().position(|c| c.id == clip_id)?;
        let was = clips[i].clone();
        // A gap beside it is room, not a neighbour.
        let left = i.checked_sub(1).map(|l| &clips[l]).filter(|c| !c.kind.is_gap()).cloned();
        let right = clips.get(i + 1).filter(|c| !c.kind.is_gap()).cloned();
        let partner = was.linked_clip_id.filter(|_| linked).and_then(|id| find(seq, id));
        Some(Self { clip_id, track_idx, was, left, right, partner, hold: None, by: None })
    }

    /// The clips the slide moves: none of them is a snap target.
    pub fn movers(&self) -> Vec<ClipId> {
        [Some(self.clip_id), self.left.as_ref().map(|c| c.id), self.right.as_ref().map(|c| c.id)].into_iter().flatten().collect()
    }

    /// The pointer `secs` along: the clip's edges snapped, then held so
    /// each neighbour keeps a frame.
    pub fn drag_to(&mut self, secs: f64, mut snap: impl Snaps) -> SlideFrame {
        let mut dt = RationalTime::from_secs_f64(secs, self.was.timeline_in.rate);
        let edges = [self.was.timeline_in + dt, self.was.timeline_out + dt];
        let found = snap(&edges, &self.movers(), &mut self.hold);
        if let Some(s) = found {
            dt += s.at - edges[s.edge];
        }
        let left = self.left.as_ref().map(|c| (c.timeline_in, c.timeline_out));
        let right = self.right.as_ref().map(|c| (c.timeline_out, c.timeline_in));
        let (new_in, _) = crate::panels::timeline::slide_clamped_window(self.was.timeline_in, self.was.timeline_out, dt, left, right);
        let by = new_in - self.was.timeline_in;
        self.by = Some(by);
        SlideFrame {
            // The line only where the slide reached the snap.
            snapped: found.filter(|_| by == dt),
            live: self.edit(by),
        }
    }

    fn edit(&self, by: Timecode) -> Option<EditCommand> {
        let mut now = self.was.clone();
        (now.timeline_in, now.timeline_out) = (self.was.timeline_in + by, self.was.timeline_out + by);
        let left = self.left.clone().map(|c| {
            let mut m = c.clone();
            m.timeline_out += by;
            m.source_out += by;
            (c, m)
        });
        let right = self.right.clone().map(|c| {
            let mut m = c.clone();
            m.timeline_in += by;
            m.source_in += by;
            (c, m)
        });
        let partner = self.partner.clone().map(|c| {
            let mut m = c.clone();
            m.timeline_in += by;
            m.timeline_out += by;
            (c, m)
        });
        edit_of("Slide", std::iter::once((self.was.clone(), now)).chain(left).chain(right).chain(partner))
    }

    /// The slide to record: the last frame's, none when it moved nothing.
    pub fn release(&self) -> Option<EditCommand> {
        self.edit(self.by?).map(|e| EditCommand::Named { label: "Slide".into(), edit: Box::new(e) })
    }

    /// The neighbours that follow it.
    pub fn left_id(&self) -> Option<ClipId> {
        self.left.as_ref().map(|c| c.id)
    }

    pub fn right_id(&self) -> Option<ClipId> {
        self.right.as_ref().map(|c| c.id)
    }

    /// The right neighbour and how far its picture moved against its
    /// start, for its angle cuts to stay on what they cut.
    pub fn right_moved(&self) -> Option<(ClipId, f64)> {
        Some((self.right.as_ref()?.id, self.by?.to_secs_f64()))
    }

    pub fn was(&self) -> &Clip {
        &self.was
    }
}

/// The edits of a roll between `a` and `b` on `seq` (a's end and b's
/// start to the times given) and, when `linked`, of their partners by the
/// same amounts, each from its own times: sound recorded apart and synced,
/// or a slipped partner, starts elsewhere (#911).
pub(crate) fn roll_edits(seq: &Sequence, a: ClipId, b: ClipId, a_out: (Timecode, Timecode), b_in: (Timecode, Timecode), linked: bool) -> Option<Vec<EditCommand>> {
    let (ca, cb) = (find(seq, a)?, find(seq, b)?);
    let mut edits = vec![EditCommand::LegacyRollTrimClip {
        clip_a_id: a,
        clip_b_id: b,
        old_a_source_out: ca.source_out,
        old_a_timeline_out: ca.timeline_out,
        old_b_source_in: cb.source_in,
        old_b_timeline_in: cb.timeline_in,
        new_a_source_out: a_out.0,
        new_a_timeline_out: a_out.1,
        new_b_source_in: b_in.0,
        new_b_timeline_in: b_in.1,
    }];
    let (a_tl, a_src) = (a_out.1 - ca.timeline_out, a_out.0 - ca.source_out);
    let (b_tl, b_src) = (b_in.1 - cb.timeline_in, b_in.0 - cb.source_in);
    let (linked_a, linked_b) = if linked { (ca.linked_clip_id, cb.linked_clip_id) } else { (None, None) };
    for c in seq.all_clips().filter(|c| Some(c.id) == linked_a || Some(c.id) == linked_b) {
        let mut m = c.clone();
        if Some(c.id) == linked_a && c.id != b {
            m.timeline_out += a_tl;
            m.source_out += a_src;
        } else if Some(c.id) == linked_b && c.id != a {
            m.timeline_in += b_tl;
            m.source_in += b_src;
        } else {
            continue;
        }
        edits.push(EditCommand::ReplaceClip { description: "Roll", old: Box::new(c.clone()), new: Box::new(m) });
    }
    Some(edits)
}

/// A roll under way: the cut between two clips moved, a's end and b's
/// start together.
pub(crate) struct RollGesture {
    pub clip_a_id: ClipId,
    pub clip_b_id: ClipId,
    pub track_idx: usize,
    pub linked: bool,
    /// The sequence as the drag found it, which each frame's roll is made
    /// from.
    original: Sequence,
    a: Clip,
    b: Clip,
    /// The file under a, where its end stops.
    media_a: Timecode,
    hold: Option<SnapHold>,
    /// Where the cut is at the last frame.
    pub cut: Option<Timecode>,
}

/// One frame of a roll: the edit to show, the snap to draw a line at, the
/// cut, and a's length there.
#[derive(Debug)]
pub(crate) struct RollFrame {
    pub live: Option<EditCommand>,
    pub snapped: Option<Snap>,
    pub cut: Timecode,
    pub a_length: Timecode,
}

impl RollGesture {
    pub fn begin(seq: &Sequence, pool: &MediaPool, track_idx: usize, a: ClipId, b: ClipId, linked: bool) -> Option<Self> {
        let track = seq.tracks.get(track_idx)?;
        let (ca, cb) = (track.find_clip(a)?.clone(), track.find_clip(b)?.clone());
        let media_a = ca.media_id().and_then(|m| pool.get(&m)).map_or(RationalTime { value: i64::MAX, rate: ca.source_out.rate }, |m| m.duration);
        Some(Self { clip_a_id: a, clip_b_id: b, track_idx, linked, original: seq.clone(), a: ca, b: cb, media_a, hold: None, cut: None })
    }

    /// The pointer `dt` along: the cut held so each clip keeps a frame and
    /// a stays inside its file, then snapped to a target both can reach.
    pub fn drag_to(&mut self, dt: RationalTime, mut snap: impl Snaps) -> RollFrame {
        let (a, b) = (&self.a, &self.b);
        let mut a_out = a.source_out + dt;
        let mut b_in = b.source_in + dt;
        let min_a_out = a.source_in + RationalTime { value: 1, rate: a.source_in.rate };
        a_out = a_out.max(min_a_out).min(self.media_a);
        let zero = RationalTime { value: 0, rate: b_in.rate };
        let max_b_in = b.source_out - RationalTime { value: 1, rate: b.source_out.rate };
        b_in = b_in.max(zero).min(max_b_in);
        // The smaller move keeps both clips whole.
        let (da, db) = (a_out - a.source_out, b_in - b.source_in);
        let delta = if da.value.abs() <= db.value.abs() { da } else { db };
        let mut cut = a.timeline_out + delta;
        let mut snapped = None;
        if let Some(s) = snap(&[cut], &[self.clip_a_id, self.clip_b_id], &mut self.hold) {
            let d = s.at - a.timeline_out;
            let (a_out, b_in) = (a.source_out + d, b.source_in + d);
            if a_out >= min_a_out && a_out <= self.media_a && b_in >= zero && b_in <= max_b_in {
                cut = s.at;
                snapped = Some(s);
            }
        }
        self.cut = Some(cut);
        RollFrame { live: self.edit(cut), snapped, cut, a_length: cut - a.timeline_in }
    }

    /// a's end and b's start with the cut at `cut`.
    fn times(&self, cut: Timecode) -> ((Timecode, Timecode), (Timecode, Timecode)) {
        let d = cut - self.a.timeline_out;
        ((self.a.source_out + d, cut), (self.b.source_in + d, cut))
    }

    fn edit(&self, cut: Timecode) -> Option<EditCommand> {
        let (a_out, b_in) = self.times(cut);
        roll_edits(&self.original, self.clip_a_id, self.clip_b_id, a_out, b_in, self.linked).map(EditCommand::Batch)
    }

    /// The roll to send, from the last frame's cut; none when the cut is
    /// where it was (a press at a cut made an empty step).
    pub fn release(&self) -> Option<klipp_core::AppCommand> {
        let cut = self.cut.filter(|c| *c != self.a.timeline_out)?;
        let ((new_a_source_out, new_a_timeline_out), (new_b_source_in, new_b_timeline_in)) = self.times(cut);
        Some(klipp_core::AppCommand::RollTrimClip { clip_a_id: self.clip_a_id, clip_b_id: self.clip_b_id, new_a_source_out, new_a_timeline_out, new_b_source_in, new_b_timeline_in, linked: self.linked })
    }

    /// Where the cut was when the drag began.
    pub fn cut_was(&self) -> Timecode {
        self.a.timeline_out
    }
}

#[cfg(test)]
#[path = "cut_gestures_tests.rs"]
mod tests;
