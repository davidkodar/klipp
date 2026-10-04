//! The default transition from the keyboard (#918): Shift+T lays a cross
//! dissolve, Alt+T an audio crossfade, at the cut nearest the playhead,
//! on the selected clip's track when it is of the right kind, else the
//! nearest on any unlocked track of that kind. Ctrl+T and Ctrl+Shift+T,
//! the keys this usually has, already make a title and open the
//! subtitles.

use super::*;

impl KlippApp {
    /// `AddDefaultTransition`: `false` for any other command.
    pub(super) fn handle_transition_key_commands(&mut self, cmd: &AppCommand) -> bool {
        let AppCommand::AddDefaultTransition { audio } = cmd else { return false };
        let want = if *audio { klipp_timeline::TrackKind::Audio } else { klipp_timeline::TrackKind::Video };
        let Some((track_index, clip_a_id, clip_b_id)) = nearest_cut(self.state.sequence(), self.state.playhead, want, self.state.selected_clip) else {
            self.state.toast.show(if *audio { "No cut between two sound clips near the playhead" } else { "No cut between two clips near the playhead" });
            return true;
        };
        let Some(spec) = klipp_core::TRANSITION_SPECS.iter().find(|s| s.string_key == "CrossDissolve") else { return true };
        self.send_commands(vec![AppCommand::AddTransition {
            track_index,
            clip_a_id,
            clip_b_id,
            kind_id: spec.kind_id,
            duration_frames: self.state.default_transition_frames,
        }]);
        true
    }
}

/// The cut nearest `playhead` between two clips that meet, on tracks of
/// `kind`: the selected clip's track only when it is one.
fn nearest_cut(
    seq: &klipp_timeline::Sequence,
    playhead: klipp_core::Timecode,
    kind: klipp_timeline::TrackKind,
    selected: Option<ClipId>,
) -> Option<(usize, ClipId, ClipId)> {
    let on_selected = selected.and_then(|id| seq.find_clip(id)).map(|(ti, _)| ti).filter(|&ti| seq.tracks[ti].kind == kind);
    let ph = playhead.to_secs_f64();
    seq.tracks
        .iter()
        .enumerate()
        .filter(|(ti, t)| t.kind == kind && !t.locked && on_selected.is_none_or(|s| s == *ti))
        .flat_map(|(ti, t)| {
            t.clips.windows(2).filter(|w| !w[0].kind.is_gap() && !w[1].kind.is_gap() && w[0].timeline_out == w[1].timeline_in).map(move |w| (ti, w[0].id, w[1].id, w[0].timeline_out))
        })
        .min_by(|a, b| (a.3.to_secs_f64() - ph).abs().total_cmp(&(b.3.to_secs_f64() - ph).abs()))
        .map(|(ti, a, b, _)| (ti, a, b))
}

#[cfg(test)]
mod tests {
    use super::nearest_cut;
    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::{Clip, Sequence, Track, TrackKind};

    #[test]
    fn the_cut_nearest_the_playhead_on_the_right_kind_of_track() {
        let tc = |s: f64| RationalTime::from_secs_f64(s, 25);
        let m = MediaId::new();
        let mut seq = Sequence::new_default();
        let mut v = Track::new("V1", TrackKind::Video);
        v.clips = vec![Clip::media(m, tc(0.0), tc(4.0), tc(0.0), tc(4.0)), Clip::media(m, tc(4.0), tc(8.0), tc(4.0), tc(8.0)), Clip::media(m, tc(8.0), tc(12.0), tc(8.0), tc(12.0))];
        let (a, b, c) = (v.clips[0].id, v.clips[1].id, v.clips[2].id);
        seq.tracks = vec![v, Track::new("A1", TrackKind::Audio)];
        assert_eq!(nearest_cut(&seq, tc(3.0), TrackKind::Video, None), Some((0, a, b)));
        assert_eq!(nearest_cut(&seq, tc(7.0), TrackKind::Video, None), Some((0, b, c)));
        assert_eq!(nearest_cut(&seq, tc(7.0), TrackKind::Audio, None), None, "no cut between sound clips");
    }
}
