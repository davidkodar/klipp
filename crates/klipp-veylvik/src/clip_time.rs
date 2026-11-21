//! Where a clip on Klipp's timeline is in its composition, and back.
//!
//! Klipp asks a clip's composition for the frame its media time falls in
//! (the source time the clip's trim, speed and direction give, at the
//! media's rate), so a composition's frames are its clip's media frames.
//! The Veylvik page opens on the frame under Klipp's playhead, shows the
//! part of the composition the clip uses, and moves Klipp's playhead to
//! where the user went in it. Everything here counts frames the way the
//! compositor does, so the page and Klipp's renders never disagree on
//! which frame a clip shows.

use klipp_core::Timecode;
use klipp_timeline::Clip;

use crate::compose::{frame_index, frame_time};
use crate::Time;

/// A clip on a timeline at `fps`, from media at `media_fps`.
#[derive(Clone, Copy)]
pub struct ClipTime<'a> {
    pub clip: &'a Clip,
    pub fps: f64,
    pub media_fps: f64,
}

impl ClipTime<'_> {
    /// The clip's timeline frames, counted at `fps` from the timeline's
    /// start: from its first to one past its last.
    fn frames(&self) -> (i64, i64) {
        let at = |t: Timecode| (t.to_secs_f64() * self.fps + 1e-6).round() as i64;
        let (first, end) = (at(self.clip.timeline_in), at(self.clip.timeline_out));
        (first, end.max(first + 1))
    }

    /// The composition frame timeline frame `frame` shows: what Klipp's
    /// renders ask the composition for there.
    fn shown(&self, frame: i64) -> i64 {
        let elapsed = frame as f64 / self.fps - self.clip.timeline_in.to_secs_f64();
        frame_index(self.clip.source_secs_at(elapsed, self.fps), self.media_fps)
    }

    /// The composition frame shown at `playhead`. A playhead off the clip
    /// (the page opened on a selected clip elsewhere) takes the clip's
    /// nearest frame.
    pub fn composition_frame(&self, playhead: Timecode) -> i64 {
        let (first, end) = self.frames();
        let at = (playhead.to_secs_f64() * self.fps + 1e-6).floor() as i64;
        self.shown(at.clamp(first, end - 1))
    }

    /// [`Self::composition_frame`] as Veylvik's exact time.
    pub fn composition_time(&self, playhead: Timecode) -> Time {
        frame_time(self.composition_frame(playhead), self.media_fps)
    }

    /// The first timeline frame that shows composition frame `frame`, or
    /// the frame past it where the clip skips it (faster than 1x). `None`
    /// for a frame the clip doesn't use, in its handles: Klipp's playhead
    /// doesn't follow there.
    pub fn timeline_frame(&self, frame: i64) -> Option<i64> {
        let (start, end) = self.used_frames();
        if frame < start || frame >= end {
            return None;
        }
        let (first, last) = self.frames();
        let reversed = self.clip.is_reversed();
        // The frames shown only move one way through the clip: those at or
        // past `frame` are a run at its end.
        let past = |f: i64| if reversed { self.shown(f) <= frame } else { self.shown(f) >= frame };
        let (mut lo, mut hi) = (first, last);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if past(mid) { hi = mid } else { lo = mid + 1 }
        }
        (lo < last).then_some(lo)
    }

    /// The timeline frame showing composition time `time`, as
    /// [`Self::timeline_frame`] does for its frame.
    pub fn timeline_frame_at(&self, time: Time) -> Option<i64> {
        self.timeline_frame(frame_index(time.seconds().ok()?, self.media_fps))
    }

    /// The composition frames the clip shows, from the first to one past
    /// the last, whichever way it plays. The rest are its handles.
    pub fn used_frames(&self) -> (i64, i64) {
        let (first, end) = self.frames();
        let (a, b) = (self.shown(first), self.shown(end - 1));
        (a.min(b), a.max(b) + 1)
    }

    /// [`Self::used_frames`] as Veylvik's exact times: the clip's working
    /// range in its composition.
    pub fn used_range(&self) -> (Time, Time) {
        let (start, end) = self.used_frames();
        (frame_time(start, self.media_fps), frame_time(end, self.media_fps))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{MediaId, RationalTime};

    fn tc(frames: i64) -> RationalTime {
        RationalTime::from_frames(frames, 24)
    }

    /// 48 frames on the timeline from frame 100, from media frame 30.
    fn clip() -> Clip {
        Clip::media(MediaId::new(), tc(100), tc(148), tc(30), tc(78))
    }

    fn at(clip: &Clip) -> ClipTime<'_> {
        ClipTime { clip, fps: 24.0, media_fps: 24.0 }
    }

    /// Every frame of a trimmed clip goes to its media frame and back to
    /// itself; its range is its trim; a frame in its handles moves nothing.
    #[test]
    fn a_trimmed_clip_maps_frame_for_frame() {
        let c = clip();
        let t = at(&c);
        for f in 100..148 {
            assert_eq!(t.composition_frame(tc(f)), 30 + f - 100, "timeline frame {f}");
            assert_eq!(t.timeline_frame(30 + f - 100), Some(f), "back from timeline frame {f}");
        }
        assert_eq!(t.used_frames(), (30, 78));
        assert_eq!(t.timeline_frame(29), None, "the handle before");
        assert_eq!(t.timeline_frame(78), None, "the handle after");
        assert_eq!(t.composition_frame(tc(20)), 30, "off the clip, its nearest frame");
        assert_eq!(t.composition_frame(tc(500)), 77);
        let (start, end) = t.used_range();
        assert_eq!((start.seconds().unwrap(), end.seconds().unwrap()), (30.0 / 24.0, 78.0 / 24.0));
        assert_eq!(t.timeline_frame_at(t.composition_time(tc(120))), Some(120), "through Veylvik's time");
    }

    /// Twice as fast: every other media frame, a range twice as long; a
    /// frame the clip skips goes to the frame showing past it.
    #[test]
    fn a_sped_up_clip_maps_through_its_speed() {
        let mut c = clip();
        c.speed = 2.0;
        c.source_out = tc(30 + 96);
        let t = at(&c);
        for f in 100..148 {
            let shown = t.composition_frame(tc(f));
            assert_eq!(shown, 30 + 2 * (f - 100), "timeline frame {f}");
            assert_eq!(t.timeline_frame(shown), Some(f));
        }
        assert_eq!(t.used_frames(), (30, 125));
        assert_eq!(t.timeline_frame(31), Some(101), "a skipped frame: the one showing past it");
    }

    /// Reversed: the clip's first frame shows the last media frame of its
    /// range, and a frame goes back to the timeline frame showing it.
    #[test]
    fn a_reversed_clip_maps_backwards() {
        let mut c = clip();
        c.speed = -1.0;
        let t = at(&c);
        assert_eq!(t.composition_frame(tc(100)), 77);
        assert_eq!(t.composition_frame(tc(147)), 30);
        for f in 100..148 {
            assert_eq!(t.timeline_frame(t.composition_frame(tc(f))), Some(f), "timeline frame {f}");
        }
        assert_eq!(t.used_frames(), (30, 78));
    }

    /// Half speed: each media frame shows on two timeline frames, and
    /// going back lands on the first of them.
    #[test]
    fn a_slowed_clip_goes_back_to_the_first_frame_showing() {
        let mut c = clip();
        c.speed = 0.5;
        c.source_out = tc(30 + 24);
        let t = at(&c);
        for f in 100..148 {
            let shown = t.composition_frame(tc(f));
            let back = t.timeline_frame(shown).expect("a frame of the clip");
            assert!(back <= f && t.composition_frame(tc(back)) == shown, "timeline frame {f} came back as {back}");
            assert!(back == 100 || t.composition_frame(tc(back - 1)) != shown, "{back} is the first showing {shown}");
        }
    }

    /// A clip from a 25 fps file on a 24 fps timeline: the composition
    /// counts the file's frames, the timeline its own, and each meets the
    /// frame showing the same picture.
    #[test]
    fn rates_that_differ_meet_on_the_frame_showing() {
        let c = clip();
        let t = ClipTime { clip: &c, fps: 24.0, media_fps: 25.0 };
        let (start, end) = t.used_frames();
        for f in 100..148 {
            let shown = t.composition_frame(tc(f));
            assert!((start..end).contains(&shown));
            let back = t.timeline_frame(shown).expect("a frame of the clip");
            assert_eq!(t.composition_frame(tc(back)), shown, "timeline frame {f} came back as {back}");
        }
    }
}
