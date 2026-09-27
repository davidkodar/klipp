//! A multicam group at a moment: its primary, its angles, and the angle
//! the primary's cut list has in effect. One answer for the viewer's
//! renderer, the multicam controls and the angle switch, which each used
//! to work it out their own way and disagreed.

use klipp_core::{GroupId, Timecode};

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
}
