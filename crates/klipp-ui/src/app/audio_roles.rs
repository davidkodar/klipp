//! Roles proposed from what clips' sound is (#2216): a sure label from
//! the audio-role analysis is applied to the clips of its file, once per
//! clip, never over a role set by hand.

use super::*;
use klipp_core::roles::RoleRef;

/// The open sequence's clips whose file's sound proposes a sure role they
/// do not have, as (clip, old role, new role): on unlocked audio tracks,
/// with a role neither set by hand nor proposed before.
pub(crate) fn role_proposals(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool) -> Vec<(klipp_core::ClipId, Option<RoleRef>, Option<RoleRef>)> {
    seq.tracks
        .iter()
        .filter(|t| t.kind == klipp_timeline::TrackKind::Audio && !t.locked)
        .flat_map(|t| &t.clips)
        .filter(|c| c.role_source == klipp_core::RoleSource::Unset)
        .filter_map(|c| {
            let p = c.media_id().and_then(|m| pool.get(&m)).and_then(|m| m.audio_role).filter(|p| p.confident)?;
            let proposed = RoleRef::from_legacy(p.role);
            (proposed != c.role).then(|| (c.id, c.role.clone(), proposed))
        })
        .collect()
}

/// The undo step that sets `clip_id`'s role to `role` by hand, keeping
/// the old role and where it came from to put back (#2229); none when the
/// clip is not in the sequence or already has that role by hand.
pub(crate) fn hand_role_edit(seq: &klipp_timeline::Sequence, clip_id: klipp_core::ClipId, role: Option<RoleRef>) -> Option<EditCommand> {
    let clip = seq.all_clips().find(|c| c.id == clip_id)?;
    (clip.role != role || clip.role_source != klipp_core::RoleSource::ByHand)
        .then(|| EditCommand::SetClipRole { clip_id, old: clip.role.clone(), new: role, old_source: clip.role_source })
}

impl KlippApp {
    /// Apply what the open sequence's sound proposes, as one undo step,
    /// when the analysis, the sequence, its history, its clips or its
    /// track locks changed. Not while there is something to redo: a step
    /// of its own would take that away, so it waits for the next edit.
    pub(super) fn propose_audio_roles(&mut self) {
        let seq = self.state.sequence();
        let clips: usize = seq.tracks.iter().map(|t| t.clips.len()).sum();
        let locked = seq.tracks.iter().filter(|t| t.locked).count();
        let key = (self.state.analysis_generation, seq.id, self.state.active_history().undo_count(), clips, locked);
        if self.state.role_sweep_key == Some(key) || self.state.active_history().redo_count() > 0 {
            return;
        }
        let changes = role_proposals(seq, &self.state.media_pool);
        self.state.role_sweep_key = Some(key);
        if !changes.is_empty() {
            let n = changes.len();
            self.apply_edits_as_one(&[EditCommand::ProposeClipRoles { changes }], "Classify Audio Roles");
            self.state.toast.show(format!("Audio roles proposed for {n} clip{} from their sound", if n == 1 { "" } else { "s" }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{ClipRole, RoleProposal, RoleSource};

    fn r(s: &str) -> Option<RoleRef> {
        RoleRef::parse(s)
    }

    /// A sure label is proposed for a clip whose role was never set; not
    /// for one set by hand, one proposed before, one on a locked track or
    /// one whose label is unsure. Undone, the clip stays as it was and is
    /// not proposed again.
    #[test]
    fn a_sure_label_is_proposed_once_and_never_over_a_hand_set_role() {
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut pool = klipp_media::MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/song.wav".into(), None);
        item.audio_role = Some(RoleProposal { role: ClipRole::Music, confidence: 0.8, confident: true });
        let song = pool.add(item);
        let mut unsure = klipp_media::MediaItem::synthetic_for_test("/tmp/room.wav".into(), None);
        unsure.audio_role = Some(RoleProposal { role: ClipRole::Ambience, confidence: 0.3, confident: false });
        let room = pool.add(unsure);
        let mut seq = klipp_timeline::Sequence::new_default();
        let audio = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio).unwrap();
        let fresh = klipp_timeline::Clip::media(song, tc(0.0), tc(4.0), tc(0.0), tc(4.0));
        let mut by_hand = klipp_timeline::Clip::media(song, tc(4.0), tc(8.0), tc(0.0), tc(4.0));
        by_hand.role_source = RoleSource::ByHand;
        let quiet = klipp_timeline::Clip::media(room, tc(8.0), tc(12.0), tc(0.0), tc(4.0));
        let id = fresh.id;
        seq.tracks[audio].clips = vec![fresh, by_hand, quiet];
        assert_eq!(role_proposals(&seq, &pool), vec![(id, None, r("music"))]);

        let edit = EditCommand::ProposeClipRoles { changes: role_proposals(&seq, &pool) };
        let mut seqs = vec![seq];
        let mut fps = klipp_core::Fps::new(24, 1);
        let mut view = klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        view.apply_forward(&edit);
        let clip = |seqs: &[klipp_timeline::Sequence]| seqs[0].tracks[audio].clips[0].clone();
        assert_eq!((clip(&seqs).role, clip(&seqs).role_source), (r("music"), RoleSource::Proposed));
        let mut view = klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        view.apply_inverse(&edit);
        assert_eq!(clip(&seqs).role, None, "undone");
        assert!(role_proposals(&seqs[0], &pool).is_empty(), "and not proposed again");

        seqs[0].tracks[audio].locked = true;
        seqs[0].tracks[audio].clips[0].role_source = RoleSource::Unset;
        assert!(role_proposals(&seqs[0], &pool).is_empty(), "a locked track is left alone");
    }

    /// #2953: a sound-only file (an MP3 with no picture) placed before its
    /// sound is heard is Music, unset, so that hearing still decides; heard
    /// as speech, it is then proposed Dialogue, and one placed after is
    /// Dialogue from the start. A role set by hand stays through both, and
    /// a file with a picture keeps the default.
    #[test]
    fn a_sound_only_file_starts_as_music_and_its_sound_decides() {
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut pool = klipp_media::MediaPool::new();
        let mut mp3 = klipp_media::MediaItem::synthetic_for_test("/tmp/song.mp3".into(), None);
        mp3.video_codec = String::new();
        let song = pool.add(mp3);
        let placed = |pool: &klipp_media::MediaPool, clip: &mut klipp_timeline::Clip| {
            crate::app::media::input_defaults_on(clip, pool.get(&clip.media_id().unwrap()).unwrap(), None, None);
        };
        let mut fresh = klipp_timeline::Clip::media(song, tc(0.0), tc(4.0), tc(0.0), tc(4.0));
        placed(&pool, &mut fresh);
        assert_eq!((fresh.role.clone(), fresh.role_source), (r("music"), RoleSource::Unset), "no speech heard yet: music");
        let mut by_hand = klipp_timeline::Clip::media(song, tc(4.0), tc(8.0), tc(0.0), tc(4.0));
        (by_hand.role, by_hand.role_source) = (r("effects"), RoleSource::ByHand);
        placed(&pool, &mut by_hand);
        assert_eq!((by_hand.role.clone(), by_hand.role_source), (r("effects"), RoleSource::ByHand), "a hand-set role stays");
        let picture = pool.add(klipp_media::MediaItem::synthetic_for_test("/tmp/shot.mov".into(), None));
        let mut shot = klipp_timeline::Clip::media(picture, tc(0.0), tc(4.0), tc(0.0), tc(4.0));
        placed(&pool, &mut shot);
        assert_eq!((shot.role, shot.role_source), (None, RoleSource::Unset), "a file with a picture: no role");

        // Heard as speech: the clip placed before is proposed Dialogue, the
        // hand-set one is not, and one placed now starts as Dialogue.
        pool.get_mut(&song).unwrap().audio_role = Some(RoleProposal { role: ClipRole::Dialogue, confidence: 0.9, confident: true });
        let mut seq = klipp_timeline::Sequence::new_default();
        let audio = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio).unwrap();
        let id = fresh.id;
        seq.tracks[audio].clips = vec![fresh, by_hand];
        assert_eq!(role_proposals(&seq, &pool), vec![(id, r("music"), r("dialogue"))]);
        let mut voice = klipp_timeline::Clip::media(song, tc(8.0), tc(12.0), tc(0.0), tc(4.0));
        placed(&pool, &mut voice);
        assert_eq!((voice.role, voice.role_source), (r("dialogue"), RoleSource::Proposed));
    }

    /// A role chosen in the inspector or a menu is one undo step (#2229):
    /// undone, the clip has its old role and where that came from back.
    #[test]
    fn a_role_set_by_hand_is_undone_with_its_source() {
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut pool = klipp_media::MediaPool::new();
        let song = pool.add(klipp_media::MediaItem::synthetic_for_test("/tmp/song.wav".into(), None));
        let mut seq = klipp_timeline::Sequence::new_default();
        let audio = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio).unwrap();
        let mut clip = klipp_timeline::Clip::media(song, tc(0.0), tc(4.0), tc(0.0), tc(4.0));
        clip.role = r("music");
        clip.role_source = RoleSource::Proposed;
        let id = clip.id;
        seq.tracks[audio].clips = vec![clip];
        assert!(hand_role_edit(&seq, klipp_core::ClipId::new(), r("effects.foley")).is_none(), "no such clip");

        let edit = hand_role_edit(&seq, id, r("effects.foley")).expect("a step");
        let mut seqs = vec![seq];
        let mut fps = klipp_core::Fps::new(24, 1);
        let at = |seqs: &[klipp_timeline::Sequence]| (seqs[0].tracks[audio].clips[0].role.clone(), seqs[0].tracks[audio].clips[0].role_source);
        let mut view = klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        view.apply_forward(&edit);
        assert_eq!(at(&seqs), (r("effects.foley"), RoleSource::ByHand));
        assert!(hand_role_edit(&seqs[0], id, r("effects.foley")).is_none(), "the same role by hand again is no step");
        assert!(hand_role_edit(&seqs[0], id, r("music")).is_some(), "another one is");
        let mut view = klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        view.apply_inverse(&edit);
        assert_eq!(at(&seqs), (r("music"), RoleSource::Proposed), "undone");
    }
}
