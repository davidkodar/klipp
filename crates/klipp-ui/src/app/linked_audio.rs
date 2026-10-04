//! Linked audio: a video clip given the sound it should have come with, and the
//! key that finds a clip's audio partner (#890).

use super::*;

/// Give an existing video clip the audio it should have come with,
/// linked, at its own position on the timeline.
///
/// The repair for a timeline built before appending brought the audio
/// along: those clips have no partner, and re-appending them to get one
/// would throw away the grades on them. Unlike
/// [`append_linked_audio_to_sequence`] this mirrors the clip exactly —
/// the same timeline span *and* the same source range — so a clip that
/// has been trimmed gets the sound that belongs under what is left of
/// it, rather than the file from the top.
///
/// `None` when there is nothing to do or it cannot be done: no such
/// clip, not on a video track, already linked to something, or no audio
/// track and none could be made.
/// A separate recording to put under a clip instead of its own sound:
/// which file, how far into it the clip's first frame falls, and how
/// long it is.
#[derive(Debug, Clone, Copy)]
pub(super) struct ExternalAudio {
    pub(super) media_id: klipp_core::MediaId,
    pub(super) offset_secs: f64,
    pub(super) len_secs: f64,
}

pub(super) fn attach_linked_audio_to_clip(
    seq: &mut klipp_timeline::Sequence,
    clip_id: klipp_core::ClipId,
    external: Option<ExternalAudio>,
) -> Option<LinkedAudioAppend> {
    let video = seq
        .tracks
        .iter()
        .find(|t| t.kind == klipp_timeline::TrackKind::Video && t.find_clip(clip_id).is_some())?
        .find_clip(clip_id)?
        .clone();
    if video.linked_clip_id.is_some() {
        return None;
    }
    let klipp_timeline::ClipKind::Media { media_id } = video.kind else { return None };
    // The recording's range under the clip: the clip's source range
    // shifted by the offset, and cut where the recording does not reach
    // — a recorder started late leaves the clip's head silent, one
    // stopped early its tail.
    let (media_id, timeline_in, timeline_out, source_in, source_out) = match external {
        None => (media_id, video.timeline_in, video.timeline_out, video.source_in, video.source_out),
        Some(ext) => {
            let rate = video.source_in.rate;
            let (mut ain, mut aout) = (video.source_in.to_secs_f64() + ext.offset_secs, video.source_out.to_secs_f64() + ext.offset_secs);
            let (mut tin, mut tout) = (video.timeline_in.to_secs_f64(), video.timeline_out.to_secs_f64());
            if ain < 0.0 {
                tin -= ain;
                ain = 0.0;
            }
            if aout > ext.len_secs {
                tout -= aout - ext.len_secs;
                aout = ext.len_secs;
            }
            if tout <= tin || aout <= ain {
                return None;
            }
            let t = |secs: f64| klipp_core::RationalTime::from_secs_f64(secs, rate);
            (ext.media_id, t(tin), t(tout), t(ain), t(aout))
        }
    };

    let (track_index, created_track_at) = match seq
        .tracks
        .iter()
        .position(|t| t.kind == klipp_timeline::TrackKind::Audio)
    {
        Some(i) => (i, None),
        None => {
            let n = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Audio).count();
            seq.add_audio_track(format!("Audio {}", n + 1));
            let i = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio)?;
            (i, Some(i))
        }
    };

    let mut audio = klipp_timeline::Clip::media(media_id, timeline_in, timeline_out, source_in, source_out);
    audio.linked_clip_id = Some(clip_id);
    let audio_id = audio.id;

    let clips_before = seq.tracks[track_index].clips.clone();
    KlippApp::overwrite_insert_clip_on_track(&mut seq.tracks[track_index], audio);
    let clips_after = seq.tracks[track_index].clips.clone();

    for track in seq.tracks.iter_mut() {
        if let Some(vc) = track.find_clip_mut(clip_id) {
            vc.linked_clip_id = Some(audio_id);
            break;
        }
    }

    let clip = seq.tracks[track_index].find_clip(audio_id)?.clone();
    Some(LinkedAudioAppend { clip, track_index, created_track_at, clips_before, clips_after })
}

/// A linked clip may have a differently nudged key in the same displayed frame.
pub(super) fn edit_linked_audio_key(keys: &mut Vec<klipp_core::Keyframe>, time: f64, tolerance: f64, insert_time: f64, value: Option<f32>) {
    let target = if tolerance > 0.0 { crate::inspector::keyframe_selection::nearest(keys, time, tolerance).map_or(insert_time, |i| keys[i].time_secs) } else { time };
    match value {
        Some(value) => klipp_core::keyframe::upsert_keyframe(keys, target, value),
        None => klipp_core::keyframe::remove_keyframe_at(keys, target),
    }
}

impl KlippApp {
    /// The recording synced to `media_id`, if any and if it is still in
    /// the pool.
    pub(super) fn external_audio_of(&self, media_id: klipp_core::MediaId) -> Option<ExternalAudio> {
        let sync = self.state.media_pool.get(&media_id)?.synced_audio.as_ref()?;
        let audio = self.state.media_pool.get(&sync.audio_media_id)?;
        Some(ExternalAudio { media_id: audio.id, offset_secs: sync.offset_secs, len_secs: audio.duration.to_secs_f64() })
    }

    /// The media's audio appended to an audio track and linked to the
    /// video clip that was just appended, as the drag-and-drop import
    /// does it.
    ///
    /// Every append path other than that import was creating the video
    /// clip alone, which is why a timeline built from the control socket
    /// — or from the media browser's append — was silent, and why every
    /// render made from one had no sound. The file's audio was never on
    /// the timeline to be mixed.
    ///
    /// `None` when there is no audio to add: audio-only media (the
    /// caller has already put it on an audio track), a silent file, or
    /// no audio track and none could be made. The caller then keeps its
    /// single-clip history entry.
    /// The media's audio, appended to an audio track and linked to the
    /// video clip that was just appended — unless source patching says
    /// video only.
    ///
    /// Positions the audio under its own video clip by delegating to
    /// `attach_linked_audio_to_clip`, and that is the whole point.
    /// Appending to the *end of the audio track* instead — which is
    /// what this did first — is only correct while the two tracks stay
    /// the same length. Any gap on the audio side and the sound lands
    /// under the wrong picture: a video-only append leaves such a gap,
    /// and so does appending a silent file, and the next clip with sound
    /// then goes in the hole rather than under itself. Found by doing
    /// exactly that on a real timeline.
    pub(super) fn append_linked_audio(
        &mut self,
        media_id: klipp_core::MediaId,
        video_clip_id: klipp_core::ClipId,
    ) -> Option<LinkedAudioAppend> {
        // Source patching: with the audio target off, an append puts the
        // picture down and nothing else. See `append_brings_audio`.
        if !self.state.append_brings_audio {
            return None;
        }
        let item = self.state.media_pool.get(&media_id)?;
        // A synced recording goes under the clip in place of its own
        // sound. Otherwise audio-only media is already on an audio
        // track, put there by the caller, and a silent file has nothing
        // to add.
        let external = self.external_audio_of(media_id);
        if external.is_none() && (!item.has_audio || item.video_codec.is_empty()) {
            return None;
        }
        let seq_idx = self.state.active_sequence_idx;
        attach_linked_audio_to_clip(&mut self.state.sequences[seq_idx], video_clip_id, external)
    }
}

/// What `append_linked_audio_to_sequence` added, in the shapes the two
/// history entries want it: `LinkedImport` takes the clip and its track,
/// `SourceLinkedInsert` takes the track's clips either side of the edit.
pub(super) struct LinkedAudioAppend {
    pub(super) clip: klipp_timeline::Clip,
    pub(super) track_index: usize,
    /// Set when the audio track had to be created for this.
    pub(super) created_track_at: Option<usize>,
    pub(super) clips_before: Vec<klipp_timeline::Clip>,
    pub(super) clips_after: Vec<klipp_timeline::Clip>,
}

#[cfg(test)]
mod attach_linked_audio_tests {
    use super::*;
    /// The bug the video-only toggle exposed, pinned.
    ///
    /// A gap on the audio track — left by a video-only append, or by
    /// appending a silent file — used to send the *next* clip's audio
    /// into the gap instead of under its own picture, because the append
    /// placed it at the end of the audio track rather than at the clip.
    /// On a real timeline that put the sound under the one clip that was
    /// meant to be silent, and left the clip that should have had it
    /// mute.
    #[test]
    fn a_gap_on_the_audio_track_does_not_misplace_the_next_clips_audio() {
        let mut seq = Sequence::new_default();
        let media_id = klipp_core::MediaId::new();
        let vi = video_track(&seq);

        // Three video clips; only the third gets audio, as if the first
        // two were appended video-only.
        let ids: Vec<_> = (0..3).map(|_| seq.tracks[vi].append_clip(media_id, t(4))).collect();
        let third = *ids.last().unwrap();
        let want = seq.tracks[vi].find_clip(third).unwrap().clone();

        let added = attach_linked_audio_to_clip(&mut seq, third, None).expect("attached");
        let audio = &seq.tracks[added.track_index].clips[0];
        assert_eq!(
            audio.timeline_in, want.timeline_in,
            "the audio went to the front of the empty audio track instead of under its clip",
        );
        assert_eq!(audio.timeline_out, want.timeline_out);
        assert_eq!(audio.linked_clip_id, Some(third));

        // And the clips that were appended video-only stay silent.
        for id in &ids[..2] {
            let c = seq.tracks[vi].find_clip(*id).unwrap();
            assert_eq!(c.linked_clip_id, None, "a video-only clip gained audio");
        }
    }

    use klipp_timeline::{Sequence, TrackKind};

    fn t(secs: i64) -> klipp_core::RationalTime {
        klipp_core::RationalTime { value: secs * 30, rate: 30 }
    }

    fn video_track(seq: &Sequence) -> usize {
        seq.tracks.iter().position(|tr| tr.kind == TrackKind::Video).unwrap()
    }

    fn audio_track(seq: &Sequence) -> Option<usize> {
        seq.tracks.iter().position(|tr| tr.kind == TrackKind::Audio)
    }

    /// The repair case: a clip already on the timeline, with a grade on
    /// it, gets its audio without being rebuilt.
    #[test]
    fn an_existing_clip_gets_audio_at_its_own_position() {
        let mut seq = Sequence::new_default();
        let media_id = klipp_core::MediaId::new();
        let vi = video_track(&seq);
        // Two clips, so the second does not start at zero.
        let _first = seq.tracks[vi].append_clip(media_id, t(4));
        let second = seq.tracks[vi].append_clip(media_id, t(4));
        let want = seq.tracks[vi].find_clip(second).unwrap().clone();
        assert!(want.timeline_in > t(0), "the fixture's second clip must not start at zero");

        let added = attach_linked_audio_to_clip(&mut seq, second, None).expect("attached");
        let audio = &seq.tracks[added.track_index].clips[0];
        assert_eq!(audio.timeline_in, want.timeline_in, "under the clip, not at the start");
        assert_eq!(audio.timeline_out, want.timeline_out);
        assert_eq!(audio.linked_clip_id, Some(second));
        let video = seq.all_clips().find(|c| c.id == second).unwrap();
        assert_eq!(video.linked_clip_id, Some(audio.id), "linked both ways");
    }

    /// A trimmed clip gets the sound under what is left of it. Taking
    /// the media from the top instead would put the audio out of sync by
    /// however much was trimmed off the front — the kind of error that
    /// looks fine until someone listens.
    #[test]
    fn a_trimmed_clip_gets_the_matching_stretch_of_sound() {
        let mut seq = Sequence::new_default();
        let media_id = klipp_core::MediaId::new();
        let vi = video_track(&seq);
        let id = seq.tracks[vi].append_clip(media_id, t(10));
        if let Some(c) = seq.tracks[vi].find_clip_mut(id) {
            c.source_in = t(3);
            c.source_out = t(7);
            c.timeline_in = t(0);
            c.timeline_out = t(4);
        }

        attach_linked_audio_to_clip(&mut seq, id, None).expect("attached");
        let ai = audio_track(&seq).unwrap();
        let audio = &seq.tracks[ai].clips[0];
        assert_eq!(audio.source_in, t(3), "the same point in the file");
        assert_eq!(audio.source_out, t(7));
        assert_eq!(audio.timeline_out - audio.timeline_in, t(4), "and the same length");
    }

    /// A synced recording goes under the clip at its offset, and is cut
    /// where the recording does not reach: a recorder started late
    /// leaves the head of the clip silent rather than sound from before
    /// the recording began.
    #[test]
    fn a_synced_recording_lies_under_the_clip_at_its_offset() {
        let mut seq = Sequence::new_default();
        let media_id = klipp_core::MediaId::new();
        let recording = klipp_core::MediaId::new();
        let vi = video_track(&seq);
        let id = seq.tracks[vi].append_clip(media_id, t(10));
        if let Some(c) = seq.tracks[vi].find_clip_mut(id) {
            c.source_in = t(2);
            c.source_out = t(7);
            c.timeline_in = t(0);
            c.timeline_out = t(5);
        }
        // The camera's first frame is 1.5 s into the recording.
        let ext = ExternalAudio { media_id: recording, offset_secs: 1.5, len_secs: 100.0 };
        attach_linked_audio_to_clip(&mut seq, id, Some(ext)).expect("attached");
        let ai = audio_track(&seq).unwrap();
        let audio = &seq.tracks[ai].clips[0];
        assert_eq!(audio.media_id(), Some(recording), "the recording, not the camera's sound");
        assert!((audio.source_in.to_secs_f64() - 3.5).abs() < 1e-6);
        assert!((audio.source_out.to_secs_f64() - 8.5).abs() < 1e-6);
        assert_eq!(audio.timeline_in, t(0));
        assert_eq!(audio.timeline_out, t(5));

        // A recorder started 3 s after the camera: the first second of the
        // clip has no recording, so the sound starts a second in.
        let mut seq = Sequence::new_default();
        let vi = video_track(&seq);
        let id = seq.tracks[vi].append_clip(media_id, t(10));
        if let Some(c) = seq.tracks[vi].find_clip_mut(id) {
            c.source_in = t(2);
            c.source_out = t(7);
            c.timeline_in = t(4);
            c.timeline_out = t(9);
        }
        let ext = ExternalAudio { media_id: recording, offset_secs: -3.0, len_secs: 100.0 };
        attach_linked_audio_to_clip(&mut seq, id, Some(ext)).expect("attached");
        let ai = audio_track(&seq).unwrap();
        let audio = &seq.tracks[ai].clips[0];
        assert!((audio.source_in.to_secs_f64()).abs() < 1e-6, "from the recording's start");
        assert!((audio.source_out.to_secs_f64() - 4.0).abs() < 1e-6);
        assert_eq!(audio.timeline_in, t(5), "a second into the clip");
        assert_eq!(audio.timeline_out, t(9));
    }

    /// Asking twice must not stack two copies of the sound.
    #[test]
    fn a_clip_that_already_has_audio_is_left_alone() {
        let mut seq = Sequence::new_default();
        let media_id = klipp_core::MediaId::new();
        let vi = video_track(&seq);
        let id = seq.tracks[vi].append_clip(media_id, t(4));

        attach_linked_audio_to_clip(&mut seq, id, None).expect("the first attach works");
        let ai = audio_track(&seq).unwrap();
        assert_eq!(seq.tracks[ai].clips.len(), 1);
        assert!(
            attach_linked_audio_to_clip(&mut seq, id, None).is_none(),
            "the second attach is refused"
        );
        assert_eq!(seq.tracks[ai].clips.len(), 1, "and adds nothing");
    }

    /// Repairing every clip of a silent timeline leaves the audio track
    /// laid out like the video track, which is the whole point.
    #[test]
    fn repairing_a_whole_timeline_mirrors_the_video_track() {
        let mut seq = Sequence::new_default();
        let media_id = klipp_core::MediaId::new();
        let vi = video_track(&seq);
        let ids: Vec<_> = (0..3).map(|_| seq.tracks[vi].append_clip(media_id, t(4))).collect();
        let spans: Vec<_> = ids
            .iter()
            .map(|id| {
                let c = seq.tracks[vi].find_clip(*id).unwrap();
                (c.timeline_in, c.timeline_out)
            })
            .collect();

        for id in &ids {
            attach_linked_audio_to_clip(&mut seq, *id, None).expect("attached");
        }
        let ai = audio_track(&seq).unwrap();
        assert_eq!(seq.tracks[ai].clips.len(), 3, "one audio clip per video clip");
        let mut got: Vec<_> =
            seq.tracks[ai].clips.iter().map(|c| (c.timeline_in, c.timeline_out)).collect();
        got.sort();
        let mut want = spans;
        want.sort();
        assert_eq!(got, want, "the audio track matches the video track's layout");
    }
}

#[cfg(test)]
mod linked_audio_frame_tests {
    use super::edit_linked_audio_key;
    use klipp_core::keyframe::upsert_keyframe;

    #[test]
    fn a_nudged_partner_key_is_edited_and_removed_at_its_own_time() {
        let mut keys = Vec::new();
        upsert_keyframe(&mut keys, 0.707, 0.5);
        upsert_keyframe(&mut keys, 0.750, 0.8);
        edit_linked_audio_key(&mut keys, 0.700, 0.5 / 24.0, 0.707, Some(0.3));
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].time_secs, 0.707);
        assert_eq!(keys[0].value, 0.3);
        assert_eq!(keys[1].value, 0.8);
        edit_linked_audio_key(&mut keys, 0.700, 0.5 / 24.0, 0.707, None);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].time_secs, 0.750);
    }
}

#[cfg(test)]
mod exact_audio_review_tests {
    use super::edit_linked_audio_key;
    #[test]
    fn exact_linked_edits_do_not_overwrite_nearby_keys() {
        let mut main = Vec::new();
        klipp_core::keyframe::upsert_keyframe(&mut main,0.707,0.5);
        let mut partner = main.clone();
        klipp_core::keyframe::upsert_keyframe(&mut main,0.700,0.3);
        edit_linked_audio_key(&mut partner,0.700,0.0,0.700,Some(0.3));
        assert_eq!(main,partner);
        assert_eq!(partner.len(),2);
        klipp_core::keyframe::remove_keyframe_at(&mut main,0.700);
        edit_linked_audio_key(&mut partner,0.700,0.0,0.700,None);
        assert_eq!(main,partner);
        assert_eq!(partner[0].time_secs,0.707);
    }
}

#[cfg(test)]
mod linked_missing_key_review_tests {
    #[test]
    fn absent_partner_key_uses_the_selected_main_timestamp() {
        let mut partner=vec![];
        super::edit_linked_audio_key(&mut partner,0.700,0.5/24.0,0.707,Some(0.25));
        assert_eq!(partner[0].time_secs,0.707);
    }
}
