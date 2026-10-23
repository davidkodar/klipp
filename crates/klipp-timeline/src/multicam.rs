//! A multicam group at a moment: its primary, its angles, and the angle
//! the primary's cut list has in effect. One answer for the viewer's
//! renderer, the multicam controls and the angle switch, which each used
//! to work it out their own way and disagreed.

use klipp_core::{GroupId, MulticamSound, Timecode};

use crate::{Clip, Sequence, TrackKind};

/// A multicam group in play at a moment.
#[derive(Debug, Clone)]
pub struct MulticamAt<'a> {
    pub group: GroupId,
    /// Angle 0, which holds the cut list.
    pub primary: &'a Clip,
    /// The index of the primary's track.
    pub primary_track: usize,
    /// The angles the group has pictures for, in order. Not assumed to be
    /// `0..n`: a group can lack one, and a count and an index then named
    /// the wrong camera.
    pub angles: Vec<u8>,
    /// The angle the cut list has in effect, when the group has it; else
    /// the primary's, 0.
    pub active: u8,
}

impl Sequence {
    /// The multicam group in play at `time`: `group`, or whichever
    /// group's primary is under `time` when `None`. The primary is found
    /// on any picture track, shown or hidden, the clip enabled or not:
    /// it is where the cut list is, whatever is shown.
    pub fn multicam_at(&self, group: Option<GroupId>, time: Timecode) -> Option<MulticamAt<'_>> {
        let under = |c: &Clip| time >= c.timeline_in && time < c.timeline_out;
        let (primary_track, primary) = self.tracks.iter().enumerate().filter(|(_, t)| t.kind != TrackKind::Audio).find_map(|(i, t)| {
            t.clips
                .iter()
                .find(|c| c.multicam_angle == 0 && c.multicam_group.is_some() && group.is_none_or(|g| c.multicam_group == Some(g)) && under(c))
                .map(|c| (i, c))
        })?;
        let group = primary.multicam_group?;
        let mut angles: Vec<u8> = self
            .tracks
            .iter()
            .filter(|t| t.kind != TrackKind::Audio)
            .flat_map(|t| t.clips.iter())
            .filter(|c| c.multicam_group == Some(group))
            .map(|c| c.multicam_angle)
            .collect();
        angles.sort_unstable();
        angles.dedup();
        let rel = (time - primary.timeline_in).to_secs_f64();
        let cut = primary.multicam_cuts.iter().rfind(|(at, _)| *at <= rel + 1e-9).map(|(_, a)| *a).unwrap_or(0);
        let active = if angles.contains(&cut) { cut } else { 0 };
        Some(MulticamAt { group, primary, primary_track, angles, active })
    }

    /// The picture clip of `angle` in `group` at `time`, enabled.
    pub fn multicam_angle_clip(&self, group: GroupId, angle: u8, time: Timecode) -> Option<&Clip> {
        self.tracks
            .iter()
            .filter(|t| t.kind != TrackKind::Audio)
            .flat_map(|t| t.clips.iter())
            .find(|c| c.multicam_group == Some(group) && c.multicam_angle == angle && c.enabled && time >= c.timeline_in && time < c.timeline_out)
    }
}

/// A primary's cut list for its clip once the picture under it has moved
/// `shift` seconds earlier against the clip's start: a blade's right half
/// (`shift` its left half's length), or a head trim. The cuts stay with
/// the picture they cut, and the angle in effect where the clip now starts
/// is kept there. Cut times are from the clip's start, so a list left as
/// it was put each cut `shift` seconds late.
pub fn rebased_cuts(cuts: &[(f64, u8)], shift: f64) -> Vec<(f64, u8)> {
    // A head made longer shows, from its new start, the angle its old
    // start had.
    let at_start = cuts.iter().rfind(|(at, _)| *at <= shift.max(0.0) + 1e-9).map_or(0, |(_, a)| *a);
    let moved = cuts.iter().map(|(at, a)| (at - shift, *a)).filter(|(at, _)| *at > 1e-9);
    // A cut to the angle already in effect cuts nothing.
    let mut on = 0;
    std::iter::once((0.0, at_start))
        .chain(moved)
        .filter(|(_, a)| {
            let changes = *a != on;
            on = *a;
            changes
        })
        .collect()
}

/// A multicam primary's cut times count from its start, so every edit
/// that makes a piece of a clip, or moves the picture under its start,
/// goes through one of these: a cut left as it was lands on another
/// picture, and the angle seen at each moment changes.
impl Clip {
    /// This clip made the piece of itself from `from` to `to` seconds
    /// after its start: a blade's half, what an insert, an overwrite or a
    /// lift leaves either side. Its cuts stay on the picture they cut.
    pub fn keep_angle_cuts_on_piece(&mut self, from: f64, to: f64) {
        if self.multicam_cuts.is_empty() {
            return;
        }
        let len = to - from;
        self.multicam_cuts = rebased_cuts(&self.multicam_cuts, from).into_iter().filter(|(at, _)| *at < len).collect();
    }

    /// The picture under this clip moved `secs` seconds earlier against
    /// its start: a head trimmed, rolled or slid, or a slip. Its cuts
    /// stay on the picture they cut.
    pub fn keep_angle_cuts_on_picture(&mut self, secs: f64) {
        if self.multicam_cuts.is_empty() {
            return;
        }
        self.multicam_cuts = rebased_cuts(&self.multicam_cuts, secs);
    }

    /// How far a slip of `source_secs` seconds of the file moves this
    /// clip's picture against its start, in seconds of the timeline: by
    /// its speed, and the other way when it plays reversed.
    pub fn picture_moved_by_slip(&self, source_secs: f64) -> f64 {
        let speed = if self.speed.is_finite() && self.speed != 0.0 { self.speed.abs() } else { 1.0 };
        let moved = source_secs / speed;
        if self.is_reversed() { -moved } else { moved }
    }
}

impl Sequence {
    /// The group and angle whose sound `clip` is: its own when it is an
    /// angle's picture, its linked picture's when it is that picture's
    /// sound on an audio track. `None` outside every group.
    pub fn multicam_sound_of(&self, clip: &Clip) -> Option<(GroupId, u8)> {
        if let Some(g) = clip.multicam_group {
            return Some((g, clip.multicam_angle));
        }
        // Linked either way round: a link is written on both clips, and
        // a project that lost one side still names its picture.
        let picture = self
            .tracks
            .iter()
            .filter(|t| t.kind != TrackKind::Audio)
            .flat_map(|t| t.clips.iter())
            .find(|c| c.multicam_group.is_some() && (Some(c.id) == clip.linked_clip_id || c.linked_clip_id == Some(clip.id)))?;
        Some((picture.multicam_group?, picture.multicam_angle))
    }

    /// The stretches of the timeline, in seconds, over which `angle` of
    /// `group` is not heard: under each of the group's primaries (a
    /// bladed group has several), wherever its sound rule picks another
    /// angle. Outside every primary an angle plays as any clip does, as
    /// its picture shows there.
    fn multicam_silences(&self, group: GroupId, angle: u8) -> Vec<(f64, f64)> {
        let pictures = || self.tracks.iter().filter(|t| t.kind != TrackKind::Audio).flat_map(|t| t.clips.iter()).filter(|c| c.multicam_group == Some(group));
        let angles: Vec<u8> = pictures().map(|c| c.multicam_angle).collect();
        let member = |a: u8| if angles.contains(&a) { a } else { 0 };
        let mut out = Vec::new();
        for p in pictures().filter(|c| c.multicam_angle == 0) {
            let (start, end) = (p.timeline_in.to_secs_f64(), p.timeline_out.to_secs_f64());
            match p.multicam_sound {
                MulticamSound::Off => out.push((start, end)),
                MulticamSound::Angle(a) => {
                    if member(a) != angle {
                        out.push((start, end));
                    }
                }
                MulticamSound::FollowPicture => out.extend(off_screen_under(p, &member, angle)),
            }
        }
        out
    }

    /// The stretches of `clip`, in timeline seconds, over which the
    /// program shows it: `None` for a clip in no multicam group, which
    /// shows wherever it is. An angle is on screen under its group's
    /// primaries only where the cut list picks it, and outside them as any
    /// clip is. What a check reads as a shot of the piece (#2417).
    pub fn multicam_shown_spans(&self, clip: &Clip) -> Option<Vec<(f64, f64)>> {
        let group = clip.multicam_group?;
        let pictures = || self.tracks.iter().filter(|t| t.kind != TrackKind::Audio).flat_map(|t| t.clips.iter()).filter(|c| c.multicam_group == Some(group));
        let angles: Vec<u8> = pictures().map(|c| c.multicam_angle).collect();
        let member = |a: u8| if angles.contains(&a) { a } else { 0 };
        let mut hidden: Vec<(f64, f64)> = pictures().filter(|c| c.multicam_angle == 0).flat_map(|p| off_screen_under(p, &member, clip.multicam_angle)).collect();
        hidden.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut shown = Vec::new();
        let (mut from, end) = (clip.timeline_in.to_secs_f64(), clip.timeline_out.to_secs_f64());
        for (a, b) in hidden {
            if a > from {
                shown.push((from, a.min(end)));
            }
            from = from.max(b);
            if from >= end {
                break;
            }
        }
        if end > from {
            shown.push((from, end));
        }
        shown.retain(|(a, b)| b > a);
        Some(shown)
    }

    /// Whether `clip`'s sound is heard at `time`: always for a clip in no
    /// multicam group. Playback asks this of each clip under the playhead.
    pub fn multicam_sounds_at(&self, clip: &Clip, time: Timecode) -> bool {
        let Some((group, angle)) = self.multicam_sound_of(clip) else { return true };
        let t = time.to_secs_f64();
        !self.multicam_silences(group, angle).iter().any(|&(a, b)| t >= a - 1e-9 && t < b - 1e-9)
    }

    /// The stretches of `clip`, in seconds from its in point, over which
    /// its sound is heard, for a render that plays the clip whole and
    /// gates it: `None` when all of it is, and empty when none. The same
    /// rule as [`Self::multicam_sounds_at`], so a render hears what
    /// playback does.
    pub fn multicam_sound_spans(&self, clip: &Clip) -> Option<Vec<(f64, f64)>> {
        let (group, angle) = self.multicam_sound_of(clip)?;
        let (start, end) = (clip.timeline_in.to_secs_f64(), clip.timeline_out.to_secs_f64());
        let mut silences: Vec<(f64, f64)> = self
            .multicam_silences(group, angle)
            .into_iter()
            .map(|(a, b)| (a.max(start), b.min(end)))
            .filter(|(a, b)| b > a)
            .collect();
        if silences.is_empty() {
            return None;
        }
        silences.sort_by(|x, y| x.0.total_cmp(&y.0));
        let mut spans = Vec::new();
        let mut from = start;
        for (a, b) in silences {
            if a > from {
                spans.push((from - start, a - start));
            }
            from = from.max(b);
        }
        if end > from {
            spans.push((from - start, end - start));
        }
        Some(spans)
    }
}

/// The stretches under primary `p`, timeline seconds, over which its cut
/// list shows another angle than `angle`; `member` maps an angle the group
/// lacks to the primary's. As `multicam_at` reads the cut list: the last
/// cut at or before a moment, the primary before the first.
fn off_screen_under(p: &Clip, member: &dyn Fn(u8) -> u8, angle: u8) -> Vec<(f64, f64)> {
    let (start, end) = (p.timeline_in.to_secs_f64(), p.timeline_out.to_secs_f64());
    let mut out = Vec::new();
    let mut from = start;
    let mut on = 0;
    for &(at, a) in &p.multicam_cuts {
        let t = (start + at).clamp(start, end);
        if t > from && member(on) != angle {
            out.push((from, t));
        }
        from = from.max(t);
        on = a;
    }
    if end > from && member(on) != angle {
        out.push((from, end));
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::{Clip, Sequence, Track, TrackKind};
    use klipp_core::{GroupId, MediaId, RationalTime};

    fn tc(s: f64) -> RationalTime {
        RationalTime::from_secs_f64(s, 24)
    }

    fn angle(g: GroupId, n: u8) -> Clip {
        let mut c = Clip::media(MediaId::new(), tc(0.0), tc(10.0), tc(0.0), tc(10.0));
        c.multicam_group = Some(g);
        c.multicam_angle = n;
        c
    }

    fn seq(clips: Vec<Clip>) -> Sequence {
        let mut s = Sequence::new_default();
        s.tracks = clips
            .into_iter()
            .enumerate()
            .map(|(i, c)| {
                let mut t = Track::new(&format!("V{i}"), TrackKind::Video);
                t.clips = vec![c];
                t
            })
            .collect();
        s
    }

    /// #2417: the stretches the program shows of each angle, as the cut
    /// list picks them; a cut to an angle the group lacks shows the
    /// primary; a clip in no group has none to say.
    #[test]
    fn the_stretches_each_angle_is_on_screen() {
        let g = GroupId::new();
        let mut primary = angle(g, 0);
        primary.multicam_cuts = vec![(2.0, 1), (5.0, 0), (7.0, 9)];
        let s = seq(vec![primary, angle(g, 1)]);
        let near = |got: Option<Vec<(f64, f64)>>, want: &[(f64, f64)]| {
            let got = got.expect("in a group");
            assert_eq!(got.len(), want.len(), "{got:?}");
            assert!(got.iter().zip(want).all(|(a, b)| (a.0 - b.0).abs() < 0.05 && (a.1 - b.1).abs() < 0.05), "{got:?} vs {want:?}");
        };
        near(s.multicam_shown_spans(&s.tracks[0].clips[0]), &[(0.0, 2.0), (5.0, 10.0)]);
        near(s.multicam_shown_spans(&s.tracks[1].clips[0]), &[(2.0, 5.0)]);
        assert_eq!(s.multicam_shown_spans(&Clip::media(MediaId::new(), tc(0.0), tc(1.0), tc(0.0), tc(1.0))), None);
    }

    /// As the creation paths make a group: picture clips only, one track
    /// each. The primary is found with nothing said about the group; a
    /// cut to an angle the group lacks leaves the primary in effect; a
    /// hidden primary track or a disabled primary still has the cut list.
    #[test]
    fn the_group_at_a_moment() {
        let g = GroupId::new();
        let mut primary = angle(g, 0);
        primary.multicam_cuts = vec![(2.0, 3), (4.0, 7)];
        primary.enabled = false;
        let mut s = seq(vec![angle(g, 3), angle(g, 1), primary]);
        s.tracks[2].visible = false;
        let m = s.multicam_at(None, tc(1.0)).expect("the group");
        assert_eq!((m.group, m.primary_track, m.angles.clone(), m.active), (g, 2, vec![0, 1, 3], 0));
        assert_eq!(s.multicam_at(None, tc(3.0)).unwrap().active, 3);
        assert_eq!(s.multicam_at(None, tc(5.0)).unwrap().active, 0, "angle 7 is not in the group");
        assert!(s.multicam_at(None, tc(11.0)).is_none());
        assert!(s.multicam_angle_clip(g, 3, tc(3.0)).is_some());
        assert!(s.multicam_angle_clip(g, 0, tc(3.0)).is_none(), "the primary is disabled");
    }

    /// #2418: a blade through a primary leaves each cut on the picture it
    /// cuts. The right half's cut times are from its own start, with the
    /// angle in effect at the blade kept from there; the angle at every
    /// moment is the one it was.
    #[test]
    fn a_blade_keeps_each_cut_on_its_picture() {
        let g = GroupId::new();
        let mut primary = angle(g, 0);
        primary.multicam_cuts = vec![(2.0, 1), (4.0, 0), (6.0, 1)];
        let mut s = seq(vec![primary, angle(g, 1)]);
        let at = |s: &Sequence| [0.5, 2.5, 3.5, 4.5, 5.5, 6.5, 9.5].map(|x| s.multicam_at(None, tc(x)).map(|m| m.active));
        let before = at(&s);
        for ti in 0..2 {
            let id = s.tracks[ti].clips[0].id;
            s.tracks[ti].split_at(id, tc(3.0)).expect("split");
        }
        assert_eq!(at(&s), before);
        assert_eq!(s.tracks[0].clips[0].multicam_cuts, vec![(2.0, 1)], "the left half keeps its own");
        assert_eq!(s.tracks[0].clips[1].multicam_cuts, vec![(0.0, 1), (1.0, 0), (3.0, 1)]);
        // A head made longer: the cuts move later, the primary from the start.
        assert_eq!(super::rebased_cuts(&[(2.0, 1)], -1.0), vec![(3.0, 1)]);
        assert_eq!(super::rebased_cuts(&[(0.0, 1), (2.0, 0)], -1.0), vec![(0.0, 1), (3.0, 0)], "the angle at the old start, from the new one");
    }

    /// Whose sound a group plays (#478). By default angle 0's, whatever
    /// the picture cuts to; an angle's sound on an audio track follows its
    /// picture through the link. Following the picture, each angle is
    /// heard where it is on screen, the spans a render gates by agreeing
    /// with what playback asks moment by moment. Off, none is heard; past
    /// the primary's end an angle plays as any clip does.
    #[test]
    fn whose_sound_a_group_plays() {
        use klipp_core::{ClipId, MulticamSound, Timecode};
        let g = GroupId::new();
        let mut primary = angle(g, 0);
        primary.timeline_out = tc(8.0);
        primary.multicam_cuts = vec![(2.0, 1), (4.0, 0), (6.0, 5)];
        let mut cam1 = angle(g, 1);
        let lav1_id = ClipId::new();
        cam1.linked_clip_id = Some(lav1_id);
        let mut s = seq(vec![primary, cam1]);
        let mut lav1 = Clip::media(MediaId::new(), tc(0.0), tc(10.0), tc(0.0), tc(10.0));
        lav1.id = lav1_id;
        lav1.linked_clip_id = Some(s.tracks[1].clips[0].id);
        let mut a = Track::new("A1", TrackKind::Audio);
        a.clips = vec![lav1];
        s.tracks.push(a);
        let at = |x: f64| -> Timecode { tc(x) };
        let clip = |s: &Sequence, t: usize| s.tracks[t].clips[0].clone();

        let (p, c1, l1) = (clip(&s, 0), clip(&s, 1), clip(&s, 2));
        assert_eq!(s.multicam_sound_of(&l1), Some((g, 1)));
        assert!(s.multicam_sounds_at(&p, at(3.0)) && !s.multicam_sounds_at(&c1, at(3.0)) && !s.multicam_sounds_at(&l1, at(3.0)));
        assert!(s.multicam_sounds_at(&l1, at(9.0)), "past the primary an angle is a clip");
        assert_eq!(s.multicam_sound_spans(&p), None);
        assert_eq!(s.multicam_sound_spans(&l1), Some(vec![(8.0, 10.0)]));

        s.tracks[0].clips[0].multicam_sound = MulticamSound::FollowPicture;
        let (p, l1) = (clip(&s, 0), clip(&s, 2));
        // Angle 5 is not in the group: the primary's from 6 s.
        assert_eq!(s.multicam_sound_spans(&p), Some(vec![(0.0, 2.0), (4.0, 8.0)]));
        assert_eq!(s.multicam_sound_spans(&l1), Some(vec![(2.0, 4.0), (8.0, 10.0)]));
        for x in [0.5, 1.5, 2.0, 3.0, 4.0, 5.0, 6.0, 6.5, 7.5, 8.5] {
            // Spans count from the clip's in point; both clips start at 0.
            let heard = |c: &Clip| s.multicam_sound_spans(c).is_none_or(|sp| sp.iter().any(|(a, b)| x >= *a && x < *b));
            if x < p.timeline_out.to_secs_f64() {
                assert_eq!(s.multicam_sounds_at(&p, at(x)), heard(&p), "primary at {x}");
            }
            assert_eq!(s.multicam_sounds_at(&l1, at(x)), heard(&l1), "lav at {x}");
        }

        s.tracks[0].clips[0].multicam_sound = MulticamSound::Angle(1);
        assert_eq!(s.multicam_sound_spans(&clip(&s, 0)), Some(vec![]));
        assert_eq!(s.multicam_sound_spans(&clip(&s, 2)), None);
        s.tracks[0].clips[0].multicam_sound = MulticamSound::Off;
        assert_eq!(s.multicam_sound_spans(&clip(&s, 2)), Some(vec![(8.0, 10.0)]));
    }
}
