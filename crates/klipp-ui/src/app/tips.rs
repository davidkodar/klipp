//! First-time tips, shown once each when what they explain first happens (#890).

use super::*;

impl KlippApp {
    /// Phase 5 — fire any one-shot first-time tips whose unlock
    /// condition is satisfied by the active sequence and that haven't
    /// already been dismissed. Each tip records its key in
    /// `prefs.general.dismissed_tips` after firing so it never fires
    /// again across sessions. Cheap per-frame check: short-circuit on
    /// the first un-dismissed, satisfied tip; usually exits at the
    /// initial `contains` lookup.
    ///
    /// Track-matte intro: fires the first time the active sequence
    /// has two video clips that overlap in time on different tracks
    /// — the moment the right-click "Use clip above as matte" gesture
    /// becomes meaningful. Dismissed forever once shown.
    pub(super) fn maybe_fire_first_time_tips(&mut self) {
        const TRACK_MATTE_TIP_KEY: &str = "track_matte_intro";
        if self.prefs.general.dismissed_tips.contains(TRACK_MATTE_TIP_KEY) {
            return;
        }
        let unlocked = sequence_has_video_track_overlap(
            &self.state.sequences[self.state.active_sequence_idx],
        );
        if !unlocked {
            return;
        }
        self.state.toast.show(
            "Tip: right-click any clip → Effects → Use Clip Above as Matte to mask one clip with another",
        );
        self.prefs.general.dismissed_tips.insert(TRACK_MATTE_TIP_KEY.to_string());
        // Best-effort save — losing the dismissed flag on a write
        // failure means the tip might fire one more time next session,
        // which is harmless. We don't propagate the error to the user.
        let _ = self.prefs.save();
    }
}

/// Phase 5 — does the sequence have at least two clips on different
/// video tracks that overlap in time? Used to gate the first-time
/// "right-click → Use clip above as matte" tip — that gesture only
/// becomes meaningful once the user has two stacked clips with a
/// shared time-range. Free fn so tests can pin behaviour without
/// standing up an `App`.
///
/// Linear in the total clip count across video tracks, with an early
/// exit on the first overlap. Empty sequences and single-track ones
/// trivially return false.
pub(super) fn sequence_has_video_track_overlap(seq: &klipp_timeline::Sequence) -> bool {
    let video_tracks: Vec<&klipp_timeline::Track> = seq
        .tracks
        .iter()
        .filter(|t| matches!(t.kind, klipp_timeline::TrackKind::Video))
        .collect();
    if video_tracks.len() < 2 {
        return false;
    }
    for (i, ti) in video_tracks.iter().enumerate() {
        for tj in video_tracks.iter().skip(i + 1) {
            for ci in &ti.clips {
                for cj in &tj.clips {
                    // Strict open-interval overlap: clips that just
                    // touch at a frame boundary (cj.in == ci.out)
                    // don't trigger — they share no actual frame.
                    if ci.timeline_in < cj.timeline_out && cj.timeline_in < ci.timeline_out {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod first_time_tip_tests {
    //! Phase 5.2 — `sequence_has_video_track_overlap` is the unlock
    //! condition for the track-matte intro tip. Tests pin the corners
    //! a regression would erode silently: empty sequence, single
    //! track, two tracks but no overlap, edge-touch (open interval),
    //! genuine overlap.
    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::{Clip, Sequence, Track, TrackKind};

    fn tc(frames: i64) -> RationalTime { RationalTime::from_frames(frames, 24) }

    fn make_clip(in_f: i64, out_f: i64) -> Clip {
        Clip::media(MediaId::new(), tc(in_f), tc(out_f), tc(0), tc(out_f - in_f))
    }

    fn video_track(name: &str, clips: Vec<Clip>) -> Track {
        let mut t = Track::new(name, TrackKind::Video);
        t.clips = clips;
        t
    }

    fn audio_track(name: &str, clips: Vec<Clip>) -> Track {
        let mut t = Track::new(name, TrackKind::Audio);
        t.clips = clips;
        t
    }

    fn seq_with_tracks(tracks: Vec<Track>) -> Sequence {
        let mut s = Sequence::new_default();
        s.tracks = tracks;
        s
    }

    #[test]
    fn no_overlap_when_sequence_is_empty() {
        let seq = seq_with_tracks(vec![]);
        assert!(!super::sequence_has_video_track_overlap(&seq));
    }

    #[test]
    fn no_overlap_when_only_one_video_track() {
        let seq = seq_with_tracks(vec![
            video_track("V1", vec![make_clip(0, 24), make_clip(24, 48)]),
        ]);
        assert!(!super::sequence_has_video_track_overlap(&seq));
    }

    #[test]
    fn no_overlap_when_audio_track_is_the_second_track() {
        // Audio track must NOT count — track-matte is a video feature.
        let seq = seq_with_tracks(vec![
            video_track("V1", vec![make_clip(0, 24)]),
            audio_track("A1", vec![make_clip(0, 24)]),
        ]);
        assert!(!super::sequence_has_video_track_overlap(&seq));
    }

    #[test]
    fn no_overlap_when_clips_just_touch_at_the_edge() {
        // Open-interval semantics: clip ending at frame 24 doesn't
        // overlap with a clip starting at frame 24.
        let seq = seq_with_tracks(vec![
            video_track("V1", vec![make_clip(0, 24)]),
            video_track("V2", vec![make_clip(24, 48)]),
        ]);
        assert!(!super::sequence_has_video_track_overlap(&seq));
    }

    #[test]
    fn overlap_when_two_video_clips_share_time_range() {
        let seq = seq_with_tracks(vec![
            video_track("V1", vec![make_clip(0, 48)]),
            video_track("V2", vec![make_clip(12, 36)]), // sits inside V1
        ]);
        assert!(super::sequence_has_video_track_overlap(&seq));
    }

    #[test]
    fn overlap_detected_across_non_adjacent_tracks() {
        // V1 + V3 overlap; V2 is empty in between. The function must
        // still find it (the gesture works across any track gap).
        let seq = seq_with_tracks(vec![
            video_track("V1", vec![make_clip(0, 48)]),
            video_track("V2", vec![]),
            video_track("V3", vec![make_clip(20, 30)]),
        ]);
        assert!(super::sequence_has_video_track_overlap(&seq));
    }
}
