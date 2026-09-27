//! Audio crossfades: a [`Transition`] on an audio track.
//!
//! On a picture track a transition names a look — a dissolve, a wipe. On
//! an audio track there is one thing it can mean, whatever kind it was
//! dropped as: the outgoing clip keeps playing past its out point into
//! its handle while the incoming one starts before its in point, the one
//! fading out as the other fades in, both on the constant-power curve so
//! the sum holds its loudness through the join (a linear crossfade dips
//! 3 dB in the middle). Nothing changes outside the transition's window.
//!
//! Playback and the render both work from these functions, so what is
//! heard in the viewer is what the export writes: the window, how far
//! each clip is extended, and which of a track's two sound lanes a clip
//! plays in.

use std::collections::HashMap;

use klipp_core::{ClipId, RationalTime, Timecode, TransitionAlignment};

use crate::{Clip, Track, Transition};

/// Where `tr` runs on the timeline, `[start, end)`, from the cut between
/// its two clips and its alignment — the same window the picture's
/// transitions use.
pub fn window(track: &Track, tr: &Transition) -> Option<(Timecode, Timecode)> {
    let b = track.find_clip(tr.clip_b_id)?;
    let cut = b.timeline_in;
    Some(match tr.alignment {
        TransitionAlignment::Center => {
            let half = RationalTime { value: tr.duration.value / 2, rate: tr.duration.rate };
            (cut - half, cut + half)
        }
        TransitionAlignment::StartAtCut => (cut, cut + tr.duration),
        TransitionAlignment::EndAtCut => (cut - tr.duration, cut),
    })
}

/// How a clip's sound is stretched and faded by the crossfades at its
/// ends. All in seconds; zero where there is no crossfade.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Extent {
    /// Sound played before the clip's in point, from its head handle.
    pub head_secs: f64,
    /// Sound played after its out point, from its tail handle.
    pub tail_secs: f64,
    /// The constant-power fade in over the first this-many seconds of
    /// the extended clip.
    pub fade_in_secs: f64,
    /// The constant-power fade out over the last this-many seconds.
    pub fade_out_secs: f64,
}

impl Extent {
    pub fn is_none(&self) -> bool {
        self.fade_in_secs <= 0.0 && self.fade_out_secs <= 0.0
    }

    /// The same extent on whole frames at `fps`, for a render that
    /// moves a clip by frames: each handle rounded to frames, and each
    /// fade over the rounded handles of both clips at that join, so the
    /// two sides still fade over one and the same span. A fade is this
    /// clip's handle plus the other clip's, so the other's is what is
    /// left of the fade once this clip's handle is taken off.
    pub fn on_frames(self, fps: f64) -> Extent {
        let frames = |secs: f64| (secs * fps).round().max(0.0);
        let secs = |n: f64| n / fps;
        let (head, tail) = (frames(self.head_secs), frames(self.tail_secs));
        let fade = |span: f64, own: f64, own_frames: f64| if span > 0.0 { secs(own_frames + frames(span - own)) } else { 0.0 };
        Extent {
            head_secs: secs(head),
            tail_secs: secs(tail),
            fade_in_secs: fade(self.fade_in_secs, self.head_secs, head),
            fade_out_secs: fade(self.fade_out_secs, self.tail_secs, tail),
        }
    }
}

/// Whether a clip's sound can fade: a player that cannot fade some
/// clip (playback's decoder fallback applies no envelope) passes one
/// of these, and a transition touching such a clip is a plain cut
/// rather than a crossfade it could not play, on both sides.
pub type CanFade<'a> = &'a dyn Fn(&Clip) -> bool;

/// Every clip can fade: the render, and the model itself.
pub const ALL: CanFade<'static> = &|_| true;

/// The two clips `tr` crossfades on `track`, when it is a crossfade: on
/// an audio track, between two enabled clips that can both fade. A
/// transition to a disabled clip is a hard cut, as a picture transition
/// to a disabled clip is.
fn crossfaded<'a>(track: &'a Track, tr: &Transition, fades: CanFade) -> Option<(&'a Clip, &'a Clip)> {
    if track.kind != crate::TrackKind::Audio {
        return None;
    }
    let (a, b) = (track.find_clip(tr.clip_a_id)?, track.find_clip(tr.clip_b_id)?);
    (a.enabled && b.enabled && fades(a) && fades(b)).then_some((a, b))
}

/// How far a crossfade reaches: where the incoming clip's sound begins,
/// where the outgoing clip's ends, and how much of each handle and how
/// long a fade that is.
struct Reach {
    from: Timecode,
    end: Timecode,
    head_secs: f64,
    tail_secs: f64,
    span_secs: f64,
}

/// The reach of the crossfade `tr` between `a` and `b`. A head is never
/// read from before the file's start: a clip whose head handle is
/// shorter than the window asks fades in from its own first sample, and
/// the crossfade is that much shorter on both sides — the outgoing
/// clip's fade out too — so the two gains still sum to unity power
/// across it. Fading the outgoing clip over the whole window while the
/// incoming one started late left a dip at the cut.
fn reach(track: &Track, tr: &Transition, a: &Clip, b: &Clip) -> Option<Reach> {
    let (start, end) = window(track, tr)?;
    let cut = b.timeline_in;
    let zero = Timecode { value: 0, rate: cut.rate };
    let head = (cut - start).min(b.source_in).max(zero);
    let tail = (end - a.timeline_out).max(zero);
    let span = end - cut + head;
    (span.value > 0).then(|| Reach {
        from: cut - head,
        end,
        head_secs: head.to_secs_f64(),
        tail_secs: tail.to_secs_f64(),
        span_secs: span.to_secs_f64(),
    })
}

/// The crossfades on `track` touching `clip`, as an [`Extent`]. Empty on
/// any track that is not an audio track, and for a crossfade whose other
/// clip is disabled (it is a hard cut then, as a picture transition to a
/// disabled clip is).
pub fn extent(track: &Track, clip: &Clip) -> Extent {
    extent_where(track, clip, ALL)
}

/// [`extent`], counting only the crossfades whose clips `fades` can fade.
pub fn extent_where(track: &Track, clip: &Clip, fades: CanFade) -> Extent {
    let mut e = Extent::default();
    for tr in &track.transitions {
        let Some((a, b)) = crossfaded(track, tr, fades) else { continue };
        let Some(r) = reach(track, tr, a, b) else { continue };
        if tr.clip_a_id == clip.id {
            e.tail_secs = r.tail_secs;
            e.fade_out_secs = r.span_secs;
        }
        if tr.clip_b_id == clip.id {
            e.head_secs = r.head_secs;
            e.fade_in_secs = r.span_secs;
        }
    }
    e
}

/// The two lanes a track's sound plays in: a crossfade needs its two
/// clips sounding at once, so each clip gets a lane, and the incoming
/// clip of a crossfade takes the lane its outgoing clip is not in.
/// Anywhere else a clip keeps the lane of the clip before it, so a plain
/// cut — two clips of one recording butted together — stays in one lane
/// and plays on without a break, as it did before crossfades existed.
///
/// Returns `0` or `1` per clip id.
pub fn lanes(track: &Track) -> HashMap<ClipId, u8> {
    lanes_where(track, ALL)
}

/// [`lanes`], alternating only across the crossfades whose clips
/// `fades` can fade: a join it cannot fade keeps its one lane, a plain
/// cut.
pub fn lanes_where(track: &Track, fades: CanFade) -> HashMap<ClipId, u8> {
    let joined: std::collections::HashSet<(ClipId, ClipId)> =
        track.transitions.iter().filter(|t| crossfaded(track, t, fades).is_some()).map(|t| (t.clip_a_id, t.clip_b_id)).collect();
    let mut out = HashMap::with_capacity(track.clips.len());
    let mut lane = 0u8;
    let mut prev: Option<ClipId> = None;
    for clip in &track.clips {
        if prev.is_some_and(|p| joined.contains(&(p, clip.id))) {
            lane ^= 1;
        }
        out.insert(clip.id, lane);
        prev = Some(clip.id);
    }
    out
}

/// A clip sounding at some moment, and how.
#[derive(Debug, Clone, Copy)]
pub struct Sounding<'a> {
    pub clip: &'a Clip,
    pub lane: u8,
    pub extent: Extent,
}

/// Every clip on `track` whose sound plays at `tc`: the clip under it,
/// and inside a crossfade's window the other clip too, playing from its
/// handle. At most two, one per lane.
pub fn sounding_at(track: &Track, tc: Timecode) -> Vec<Sounding<'_>> {
    sounding_at_where(track, tc, ALL)
}

/// [`sounding_at`] for a player that can fade only the clips `fades`
/// says: a crossfade touching any other clip is a plain cut to it, one
/// clip in one lane, neither extended. Playing both clips at full level
/// and then stopping the outgoing one dead at the window's end was
/// worse than the cut.
pub fn sounding_at_where<'a>(track: &'a Track, tc: Timecode, fades: CanFade) -> Vec<Sounding<'a>> {
    let mut clips: Vec<&Clip> = Vec::with_capacity(2);
    if let Some((_, c)) = track.clip_at_timecode(tc) {
        clips.push(c);
    }
    for tr in &track.transitions {
        let Some((a, b)) = crossfaded(track, tr, fades) else { continue };
        let Some(r) = reach(track, tr, a, b) else { continue };
        if tc < r.from || tc >= r.end {
            continue;
        }
        for c in [a, b] {
            if !clips.iter().any(|k| k.id == c.id) {
                clips.push(c);
            }
        }
    }
    if clips.is_empty() {
        return Vec::new();
    }
    let lanes = lanes_where(track, fades);
    clips
        .into_iter()
        .map(|clip| Sounding { clip, lane: lanes.get(&clip.id).copied().unwrap_or(0), extent: extent_where(track, clip, fades) })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TrackKind, TransitionKind};
    use klipp_core::{MediaId, TransitionId};

    const R: u32 = 25;

    fn t(frames: i64) -> Timecode {
        RationalTime { value: frames, rate: R }
    }

    fn track(kind: TrackKind) -> Track {
        let mut tr = Track::new("A1", kind);
        let media = MediaId::new();
        // Two 4 s clips butted at 4 s, each with 2 s of handle either side.
        tr.clips.push(Clip::media(media, t(0), t(100), t(50), t(150)));
        tr.clips.push(Clip::media(media, t(100), t(200), t(250), t(350)));
        tr
    }

    fn crossfade(tr: &mut Track, frames: i64, alignment: TransitionAlignment) {
        let (a, b) = (tr.clips[0].id, tr.clips[1].id);
        tr.transitions.push(Transition {
            id: TransitionId::new(),
            kind: TransitionKind::CrossDissolve,
            duration: t(frames),
            clip_a_id: a,
            clip_b_id: b,
            user_params: Default::default(),
            alignment,
        });
    }

    #[test]
    fn a_centred_crossfade_extends_both_clips_by_half_and_fades_over_the_whole() {
        let mut tr = track(TrackKind::Audio);
        crossfade(&mut tr, 20, TransitionAlignment::Center);
        let (a, b) = (&tr.clips[0], &tr.clips[1]);
        assert_eq!(extent(&tr, a), Extent { head_secs: 0.0, tail_secs: 0.4, fade_in_secs: 0.0, fade_out_secs: 0.8 });
        assert_eq!(extent(&tr, b), Extent { head_secs: 0.4, tail_secs: 0.0, fade_in_secs: 0.8, fade_out_secs: 0.0 });
        let lanes = lanes(&tr);
        assert_ne!(lanes[&a.id], lanes[&b.id]);
    }

    #[test]
    fn inside_the_window_both_clips_sound_and_outside_only_one() {
        let mut tr = track(TrackKind::Audio);
        crossfade(&mut tr, 20, TransitionAlignment::Center);
        assert_eq!(sounding_at(&tr, t(85)).len(), 1);
        let inside = sounding_at(&tr, t(95));
        assert_eq!(inside.len(), 2);
        assert_ne!(inside[0].lane, inside[1].lane);
        assert_eq!(sounding_at(&tr, t(105)).len(), 2);
        let after = sounding_at(&tr, t(110));
        assert_eq!(after.len(), 1);
        // The incoming clip keeps the lane it crossfaded in on.
        assert_eq!(after[0].lane, inside.iter().find(|s| s.clip.id == tr.clips[1].id).unwrap().lane);
    }

    #[test]
    fn a_plain_cut_stays_in_one_lane_and_a_picture_track_never_crossfades() {
        let tr = track(TrackKind::Audio);
        let lanes = lanes(&tr);
        assert_eq!(lanes[&tr.clips[0].id], lanes[&tr.clips[1].id]);
        let mut video = track(TrackKind::Video);
        crossfade(&mut video, 20, TransitionAlignment::Center);
        assert!(extent(&video, &video.clips[0]).is_none());
        assert_eq!(sounding_at(&video, t(95)).len(), 1);
    }

    #[test]
    fn alignment_decides_which_side_of_the_cut_the_fade_takes() {
        let mut start = track(TrackKind::Audio);
        crossfade(&mut start, 20, TransitionAlignment::StartAtCut);
        assert_eq!(extent(&start, &start.clips[0]).tail_secs, 0.8);
        assert_eq!(extent(&start, &start.clips[1]).head_secs, 0.0);
        let mut end = track(TrackKind::Audio);
        crossfade(&mut end, 20, TransitionAlignment::EndAtCut);
        assert_eq!(extent(&end, &end.clips[0]).tail_secs, 0.0);
        assert_eq!(extent(&end, &end.clips[1]).head_secs, 0.8);
    }

    #[test]
    fn a_join_the_player_cannot_fade_is_a_plain_cut() {
        let mut tr = track(TrackKind::Audio);
        crossfade(&mut tr, 20, TransitionAlignment::Center);
        let b_id = tr.clips[1].id;
        let fades: CanFade = &|c: &Clip| c.id != b_id;
        let inside = sounding_at_where(&tr, t(95), fades);
        assert_eq!(inside.len(), 1);
        assert_eq!(inside[0].clip.id, tr.clips[0].id);
        assert!(inside[0].extent.is_none());
        let after = sounding_at_where(&tr, t(105), fades);
        assert_eq!((after.len(), after[0].clip.id, after[0].lane), (1, b_id, inside[0].lane));
        assert!(extent_where(&tr, &tr.clips[1], fades).is_none());
        // With both fading it is the crossfade it was.
        assert_eq!(sounding_at_where(&tr, t(95), ALL).len(), 2);
    }

    #[test]
    fn on_frames_keeps_the_two_sides_of_a_join_on_one_span() {
        // A 30-rate clip on a 25 fps sequence: a 24-frame centred
        // crossfade's half is 14 frames at 30 (0.4667 s), not a whole
        // frame at 25. Each handle rounds to 12 frames, and both fades
        // run over the two rounded handles: 24 frames.
        let e = Extent { head_secs: 14.0 / 30.0, tail_secs: 0.0, fade_in_secs: 28.0 / 30.0, fade_out_secs: 0.0 }.on_frames(25.0);
        assert!((e.head_secs - 0.48).abs() < 1e-9 && (e.fade_in_secs - 0.96).abs() < 1e-9, "{e:?}");
        let e = Extent { head_secs: 0.0, tail_secs: 14.0 / 30.0, fade_in_secs: 0.0, fade_out_secs: 28.0 / 30.0 }.on_frames(25.0);
        assert!((e.tail_secs - 0.48).abs() < 1e-9 && (e.fade_out_secs - 0.96).abs() < 1e-9, "{e:?}");
        assert_eq!(Extent::default().on_frames(25.0), Extent::default());
    }

    #[test]
    fn a_head_is_never_read_from_before_the_file_began() {
        let mut tr = track(TrackKind::Audio);
        tr.clips[1].source_in = t(5);
        crossfade(&mut tr, 20, TransitionAlignment::Center);
        let (a, b) = (extent(&tr, &tr.clips[0]), extent(&tr, &tr.clips[1]));
        assert!((b.head_secs - 0.2).abs() < 1e-9);
        // Both sides fade over the shortened window, 0.2 s before the
        // cut to 0.4 s after it, so their gains still sum to unity
        // power; and the incoming clip is not heard before it.
        assert!((b.fade_in_secs - 0.6).abs() < 1e-9 && (a.fade_out_secs - 0.6).abs() < 1e-9 && (a.tail_secs - 0.4).abs() < 1e-9, "{a:?} {b:?}");
        assert_eq!(sounding_at(&tr, t(93)).len(), 1);
        assert_eq!(sounding_at(&tr, t(96)).len(), 2);
    }
}
