//! What dragging a clip's edge means as an edit (U2): where the edge
//! lands, snapped and held inside the file, and the trim it makes there,
//! planned by the document's own trim on the sequence as the drag found
//! it. No painting and no pointer: the timeline hands over how far the
//! pointer has gone and shows what comes back.
//!
//! Each frame gives the whole trim from the start, for the project to
//! show as its live edit in place of the last. Letting go gives the trim
//! to send, and where a stretch would go if the drag ran past the media.

use klipp_core::{ClipId, Fps, RationalTime, Timecode};
use klipp_document::history::EditCommand;
use klipp_document::ops::Edge;
use klipp_media::MediaPool;
use klipp_timeline::{Clip, Sequence};

use klipp_core::AppCommand;
use crate::timeline::snap::{Snap, SnapHold};

/// An edge drag under way.
pub(crate) struct TrimGesture {
    pub clip_id: ClipId,
    pub track_idx: usize,
    pub edge: Edge,
    pub ripple: bool,
    /// Whether the linked partner follows (#916): Alt held when the trim
    /// began, or the Linked switch off, trims this clip alone.
    pub linked: bool,
    /// The sequence as the drag found it, which every frame's trim is
    /// planned on.
    original: Sequence,
    /// The clip as the drag found it.
    was: Clip,
    /// Where the edge last landed with a plan.
    pub target: Option<Timecode>,
    /// The trim reached the end of the clip's media (or its start) and
    /// the pointer went on (#3055: where a stretch is offered).
    pub at_media_end: bool,
    /// Where the pointer would have the edge, past the media's end too:
    /// where a stretch instead goes.
    pub wanted: Option<Timecode>,
    /// The snap the drag holds from frame to frame (#3056).
    hold: Option<SnapHold>,
}

/// One frame of the drag: the trim to show (none when this place is
/// refused, so the last one stays), and the snap to draw a line at.
#[derive(Debug, Default)]
pub(crate) struct TrimFrame {
    pub live: Option<EditCommand>,
    pub snapped: Option<Snap>,
}

impl TrimGesture {
    /// An edge drag of clip `clip_id` on track `track_idx` of `seq`.
    pub fn begin(seq: &Sequence, track_idx: usize, clip_id: ClipId, edge: Edge, ripple: bool, linked: bool) -> Option<Self> {
        let was = seq.tracks.get(track_idx)?.find_clip(clip_id)?.clone();
        Some(Self { clip_id, track_idx, edge, ripple, linked, original: seq.clone(), was, target: None, at_media_end: false, wanted: None, hold: None })
    }

    /// The sequence as the drag found it.
    pub fn original(&self) -> &Sequence {
        &self.original
    }

    /// The clip as the drag found it.
    pub fn was(&self) -> &Clip {
        &self.was
    }

    /// The pointer `dt` along from where the drag began. `snap` snaps one
    /// edge, leaving out the clip named, through the hold it is given.
    pub fn drag_to(&mut self, dt: RationalTime, fps: Fps, pool: &MediaPool, mut snap: impl FnMut(RationalTime, ClipId, &mut Option<SnapHold>) -> Option<Snap>) -> TrimFrame {
        let mut frame = TrimFrame::default();
        let was = &self.was;
        let (head_min, tail_max) = trim_limits(was, pool);
        let one_frame = klipp_timeline::one_frame(fps).to_secs_f64();
        let clip_id = self.clip_id;
        let mut find = |at: RationalTime, hold: &mut Option<SnapHold>| snap(at, clip_id, hold);
        // The edge as the pointer has it, snapped where it is drawn (a
        // ripple head trim draws its end moving), then held inside the
        // file and a frame of length.
        let ripple_head = self.edge == Edge::In && self.ripple;
        let mut target = match self.edge {
            Edge::In if self.ripple => match find(was.timeline_out - dt, &mut self.hold) {
                Some(s) => {
                    frame.snapped = Some(s);
                    was.timeline_in + (was.timeline_out - s.at)
                }
                None => was.timeline_in + dt,
            },
            Edge::In => was.timeline_in + dt,
            Edge::Out => was.timeline_out + dt,
        };
        if !ripple_head {
            if let Some(s) = find(target, &mut self.hold) {
                target = s.at;
                frame.snapped = Some(s);
            }
        }
        // The stretch offered at the media's end goes where the start was
        // dragged, snapped as a start edge snaps: a ripple head trim snaps
        // its drawn end instead, and the stretch handed that start ran a
        // frame into the clip before and was refused (#3124).
        let offered = if ripple_head {
            let start = was.timeline_in + dt;
            find(start, &mut self.hold).map_or(start, |s| s.at)
        } else {
            target
        };
        // Held at the next shot on the track, where a stretch without
        // ripple stops: offered past it, unsnapped, the stretch was
        // refused, its refusal gone before it was read (#3125). A gap
        // clip is room, not a shot.
        let shots: Vec<_> = self.original.tracks.get(self.track_idx).map(|t| t.clips.iter().filter(|c| c.id != self.clip_id && !c.kind.is_gap()).map(|c| (c.timeline_in, c.timeline_out)).collect()).unwrap_or_default();
        self.wanted = Some(match self.edge {
            Edge::In => shots.iter().map(|s| s.1).filter(|end| *end <= was.timeline_in).max().map_or(offered, |end| offered.max(end)),
            Edge::Out => shots.iter().map(|s| s.0).filter(|start| *start >= was.timeline_out).min().map_or(offered, |start| offered.min(start)),
        });
        let t = target.to_secs_f64();
        let (lo, hi) = match self.edge {
            Edge::In => (head_min.max(if self.ripple { f64::NEG_INFINITY } else { 0.0 }), was.timeline_out.to_secs_f64() - one_frame),
            Edge::Out => (was.timeline_in.to_secs_f64() + one_frame, tail_max),
        };
        // Only a clip playing a file runs out of it; a title's start stops
        // at the timeline's.
        let plays = klipp_document::ops::plays_at_speed(was, pool);
        self.at_media_end = plays && match self.edge {
            Edge::In => t < head_min,
            Edge::Out => t > tail_max,
        };
        if t < lo || t > hi {
            target = RationalTime::from_secs_f64(t.clamp(lo, hi), target.rate);
        }
        // Refused (a ramp, a locked partner): the last place that planned
        // stays shown, and release says why.
        if let Ok(edit) = klipp_document::ops::trim_at_linked(&self.original, pool, self.clip_id, self.edge, target, self.ripple, self.linked) {
            self.target = Some(target);
            frame.live = Some(edit);
        }
        frame
    }

    /// Let go: the trim to send, from where the edge last landed with a
    /// plan, in the times `clip_edits::trim` reads (a timeline move of the
    /// edge, one to one with its source). None when it never planned.
    pub fn release(&self) -> Option<AppCommand> {
        let target = self.target?;
        let w = &self.was;
        let (mut new_si, mut new_so, mut new_ti, mut new_to) = (w.source_in, w.source_out, w.timeline_in, w.timeline_out);
        match self.edge {
            Edge::In => {
                new_si = w.source_in + (target - w.timeline_in);
                if !self.ripple {
                    new_ti = target;
                }
            }
            Edge::Out => {
                new_to = target;
                new_so = w.source_out + (target - w.timeline_out);
            }
        }
        Some(AppCommand::TrimClip {
            clip_id: self.clip_id,
            old_source_in: w.source_in,
            old_source_out: w.source_out,
            old_timeline_in: w.timeline_in,
            old_timeline_out: w.timeline_out,
            new_source_in: new_si,
            new_source_out: new_so,
            new_timeline_in: new_ti,
            new_timeline_out: new_to,
            ripple: self.ripple,
            linked: self.linked,
        })
    }
}

/// How far a clip's edges can be trimmed on the timeline: its start back
/// to where its file begins and its end on to where the file ends, read
/// through its speed and, reversed, from the file's other end. A title,
/// still or generator has no file to run out of.
pub(crate) fn trim_limits(clip: &Clip, pool: &MediaPool) -> (f64, f64) {
    let media = clip.media_id().and_then(|m| pool.get(&m)).filter(|m| !m.is_still).map(|m| m.duration.to_secs_f64());
    let Some(file) = media else { return (0.0, f64::INFINITY) };
    let speed = if clip.speed.is_finite() && clip.speed != 0.0 { clip.speed.abs() } else { 1.0 };
    let (before, after) = (clip.source_in.to_secs_f64(), file - clip.source_out.to_secs_f64());
    let (head, tail) = if clip.is_reversed() { (after, before) } else { (before, after) };
    (clip.timeline_in.to_secs_f64() - head / speed, clip.timeline_out.to_secs_f64() + tail / speed)
}

#[cfg(test)]
#[path = "trim_gesture_tests.rs"]
mod tests;
