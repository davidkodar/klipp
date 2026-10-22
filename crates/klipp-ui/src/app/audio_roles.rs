//! Roles proposed from what clips' sound is (#2216): a sure label from
//! the audio-role analysis is applied to the clips of its file, once per
//! clip, never over a role set by hand.

use super::*;

/// The open sequence's clips whose file's sound proposes a sure role they
/// do not have, as (clip, old role, new role): on unlocked audio tracks,
/// with a role neither set by hand nor proposed before.
pub(crate) fn role_proposals(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool) -> Vec<(klipp_core::ClipId, klipp_core::ClipRole, klipp_core::ClipRole)> {
    seq.tracks
        .iter()
        .filter(|t| t.kind == klipp_timeline::TrackKind::Audio && !t.locked)
        .flat_map(|t| &t.clips)
        .filter(|c| c.role_source == klipp_core::RoleSource::Unset)
        .filter_map(|c| {
            let p = c.media_id().and_then(|m| pool.get(&m)).and_then(|m| m.audio_role).filter(|p| p.confident)?;
            (p.role != c.role).then_some((c.id, c.role, p.role))
        })
        .collect()
}

impl KlippApp {
    /// Apply what the open sequence's sound proposes, as one undo step,
    /// when the analysis, the sequence, its history or its clips changed.
    pub(super) fn propose_audio_roles(&mut self) {
        let seq = self.state.sequence();
        let key = (self.state.analysis_generation, seq.id, self.state.active_history().undo_count(), seq.tracks.iter().map(|t| t.clips.len()).sum());
        if self.state.role_sweep_key == Some(key) {
            return;
        }
        let changes = role_proposals(seq, &self.state.media_pool);
        self.state.role_sweep_key = Some(key);
        if !changes.is_empty() {
            self.apply_edits_as_one(&[EditCommand::ProposeClipRoles { changes }], "Classify Audio Roles");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{ClipRole, RoleProposal, RoleSource};

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
        assert_eq!(role_proposals(&seq, &pool), vec![(id, ClipRole::Dialogue, ClipRole::Music)]);

        let edit = EditCommand::ProposeClipRoles { changes: role_proposals(&seq, &pool) };
        let mut seqs = vec![seq];
        let mut fps = klipp_core::Fps::new(24, 1);
        let mut view = klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        view.apply_forward(&edit);
        let clip = |seqs: &[klipp_timeline::Sequence]| seqs[0].tracks[audio].clips[0].clone();
        assert_eq!((clip(&seqs).role, clip(&seqs).role_source), (ClipRole::Music, RoleSource::Proposed));
        let mut view = klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        view.apply_inverse(&edit);
        assert_eq!(clip(&seqs).role, ClipRole::Dialogue, "undone");
        assert!(role_proposals(&seqs[0], &pool).is_empty(), "and not proposed again");

        seqs[0].tracks[audio].locked = true;
        seqs[0].tracks[audio].clips[0].role_source = RoleSource::Unset;
        assert!(role_proposals(&seqs[0], &pool).is_empty(), "a locked track is left alone");
    }
}
