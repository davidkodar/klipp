//! A sequence's version at another aspect (#2211): its files' framing
//! analysis asked for, and the version reframed once that is done.

use super::*;

/// The aspects a version can be made at, as width and height parts.
pub(crate) const ASPECTS: [(&str, u32, u32); 4] = [("9:16", 9, 16), ("1:1", 1, 1), ("4:5", 4, 5), ("16:9", 16, 9)];

/// A frame of `aspect` keeping `frame`'s short side as its own, both
/// sides even: 1920x1080 is 1080x1920 at 9:16, 1080x1080 at 1:1 and
/// 1080x1350 at 4:5.
pub(crate) fn aspect_size(frame: klipp_core::Resolution, aspect: (u32, u32)) -> (u32, u32) {
    let short = frame.width.min(frame.height).max(2) as f64;
    let (a, b) = (aspect.0.max(1) as f64, aspect.1.max(1) as f64);
    let even = |v: f64| ((v / 2.0).round() as u32).max(1) * 2;
    if a <= b { (even(short), even(short * b / a)) } else { (even(short * a / b), even(short)) }
}

/// Whether `frame` already has `aspect`, to a pixel.
pub(crate) fn has_aspect(frame: klipp_core::Resolution, aspect: (u32, u32)) -> bool {
    (frame.width as u64 * aspect.1 as u64).abs_diff(frame.height as u64 * aspect.0 as u64) <= aspect.0.max(aspect.1) as u64
}

/// A copy of sequence `source` at `size`, named `name`, added after the
/// others: its index. The pictures are reframed later, as one undo step.
pub(super) fn add_version(state: &mut crate::state::AppState, source: usize, size: (u32, u32), name: &str) -> Option<usize> {
    let mut version = state.sequences.get(source)?.clone();
    version.id = klipp_core::SequenceId::new();
    version.name = name.to_string();
    let from = version.resolution;
    version.resolution = klipp_core::Resolution { width: size.0, height: size.1 };
    // Each reframe on its picture in the new frame, for the refit to keep.
    klipp_document::ops::keep_reframes(&mut version, &state.media_pool, from);
    version.awaiting_refit = true;
    state.sequences.push(version);
    state.dirty = true;
    Some(state.sequences.len() - 1)
}

/// Which of `pending` to reframe now, given the sequences there are, the
/// one open and whether a file is still being analysed: those whose files
/// are done, once open. The rest are kept, less the files that are done,
/// and those whose sequence is gone are dropped.
pub(super) fn due_refits(pending: &mut Vec<crate::state::PendingRefit>, exists: impl Fn(klipp_core::SequenceId) -> bool, open: Option<klipp_core::SequenceId>, busy: impl Fn(klipp_core::MediaId) -> bool) -> Vec<klipp_core::SequenceId> {
    let mut due = Vec::new();
    pending.retain_mut(|p| {
        if !exists(p.sequence) {
            return false;
        }
        p.waiting.retain(|id| busy(*id));
        if p.waiting.is_empty() && open == Some(p.sequence) {
            due.push(p.sequence);
            return false;
        }
        true
    });
    due
}

/// The waits of an opened project's aspect versions still to be reframed
/// (#2231): their files' framing asked for again.
pub(super) fn waiting_refits(sequences: &[klipp_timeline::Sequence], pool: &klipp_media::MediaPool) -> Vec<crate::state::PendingRefit> {
    sequences.iter().filter(|s| s.awaiting_refit).map(|s| crate::state::PendingRefit { sequence: s.id, waiting: request_framing(s, pool) }).collect()
}

/// Ask for the framing analysis of every file `seq` shows a picture of,
/// with subject detection on: the files still being analysed.
pub(super) fn request_framing(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool) -> Vec<klipp_core::MediaId> {
    if !klipp_analysis::subject_detection_enabled() {
        return Vec::new();
    }
    let mut waiting = Vec::new();
    let pictures = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video && !t.locked).flat_map(|t| &t.clips);
    for id in pictures.filter_map(|c| c.media_id()) {
        let Some(item) = pool.get(&id).filter(|m| m.resolution.width > 0 && !waiting.contains(&id)) else { continue };
        let status = klipp_analysis::service::request(item);
        if matches!(status.state, klipp_analysis::service::State::Queued | klipp_analysis::service::State::Running) {
            waiting.push(id);
        }
    }
    waiting
}

impl KlippApp {
    /// Reframe each aspect version whose files are analysed, once it is
    /// the sequence open; one whose sequence was deleted is let go.
    pub(super) fn advance_refits(&mut self) {
        if self.state.pending_refits.is_empty() {
            return;
        }
        let open = self.state.sequences.get(self.state.active_sequence_idx).map(|s| s.id);
        let ids: Vec<_> = self.state.sequences.iter().map(|s| s.id).collect();
        let busy = |id| klipp_analysis::service::status(id).is_some_and(|s| matches!(s.state, klipp_analysis::service::State::Queued | klipp_analysis::service::State::Running));
        // The open sequence is the one an op resolves on, so one due at
        // most.
        for id in due_refits(&mut self.state.pending_refits, |id| ids.contains(&id), open, busy) {
            if let Some(seq) = self.state.sequences.iter_mut().find(|s| s.id == id) {
                seq.awaiting_refit = false;
            }
            self.state.pending_doc_ops.push(crate::state::DocOpsRequest {
                label: "Refit to Frame".into(),
                ops: vec![klipp_document::ops::Op::Refit { markers: true, punch: None }],
                independent: false,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_keeps_the_short_side() {
        let hd = klipp_core::Resolution { width: 1920, height: 1080 };
        assert_eq!(aspect_size(hd, (9, 16)), (1080, 1920));
        assert_eq!(aspect_size(hd, (1, 1)), (1080, 1080));
        assert_eq!(aspect_size(hd, (4, 5)), (1080, 1350));
        let tall = klipp_core::Resolution { width: 1080, height: 1920 };
        assert_eq!(aspect_size(tall, (16, 9)), (1920, 1080));
        assert!(has_aspect(hd, (16, 9)) && !has_aspect(hd, (9, 16)));
        assert!(has_aspect(klipp_core::Resolution { width: 3840, height: 2160 }, (16, 9)));
    }

    /// A version is the sequence copied at its new size, added last; the
    /// one it was made from is left as it was.
    #[test]
    fn a_version_is_a_copy_at_the_new_size() {
        let mut state = crate::state::AppState::default();
        let source = state.sequences[0].clone();
        let index = add_version(&mut state, 0, (1080, 1920), "Cut 9:16").unwrap();
        assert_eq!(index, 1);
        let version = &state.sequences[1];
        assert_eq!((version.name.as_str(), version.resolution.width, version.resolution.height), ("Cut 9:16", 1080, 1920));
        assert_ne!(version.id, source.id);
        assert_eq!(version.tracks.len(), source.tracks.len());
        assert_eq!(state.sequences[0].resolution, source.resolution);
        assert!(add_version(&mut state, 9, (1, 1), "x").is_none());
        assert!(state.sequences[1].awaiting_refit && !state.sequences[0].awaiting_refit, "the version waits to be reframed");
    }

    /// An opened project's versions still to be reframed wait again
    /// (#2231); the rest do not.
    #[test]
    fn an_opened_version_still_to_be_reframed_waits_again() {
        let mut state = crate::state::AppState::default();
        add_version(&mut state, 0, (1080, 1920), "Cut 9:16").unwrap();
        let pending = waiting_refits(&state.sequences, &state.media_pool);
        assert_eq!(pending.iter().map(|p| p.sequence).collect::<Vec<_>>(), vec![state.sequences[1].id]);
        state.sequences[1].awaiting_refit = false;
        assert!(waiting_refits(&state.sequences, &state.media_pool).is_empty());
    }

    /// A version is reframed once its files are analysed and it is the
    /// sequence open, however the tabs were switched meanwhile; one whose
    /// sequence was deleted is let go.
    #[test]
    fn a_version_is_reframed_once_analysed_and_open() {
        use crate::state::PendingRefit;
        let (a, b) = (klipp_core::SequenceId::new(), klipp_core::SequenceId::new());
        let file = klipp_core::MediaId::new();
        let mut pending = vec![PendingRefit { sequence: a, waiting: vec![file] }];
        let all = |_| true;
        assert!(due_refits(&mut pending, all, Some(a), |_| true).is_empty(), "still analysing");
        assert!(due_refits(&mut pending, all, Some(b), |_| false).is_empty(), "analysed, but another tab is open");
        assert!(pending[0].waiting.is_empty(), "the file done is crossed off");
        assert_eq!(due_refits(&mut pending, all, Some(a), |_| true), vec![a], "open again: reframed");
        assert!(pending.is_empty());
        let mut gone = vec![PendingRefit { sequence: b, waiting: vec![] }];
        assert!(due_refits(&mut gone, |_| false, Some(b), |_| false).is_empty());
        assert!(gone.is_empty(), "its sequence was deleted");
    }
}
