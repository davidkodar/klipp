//! The editing helpers' dialog (#988): strobe, set length and a clip over
//! every edit, each turned into a document op and run as one undo step.

use klipp_core::ClipId;
use klipp_document::ops::{Anchor, ClipRef, EditAlign, LengthKeep, Op};
use klipp_timeline::Sequence;

/// The dialog's settings, kept while it is open.
#[derive(Debug, Clone, PartialEq)]
pub struct EditHelperDialog {
    /// 0 Strobe, 1 Set Length, 2 Copy Over Every Edit.
    pub kind: u8,
    pub clip_id: ClipId,
    pub piece_frames: i64,
    pub on: u32,
    pub off: u32,
    pub ripple: bool,
    pub frames: i64,
    pub keep: LengthKeep,
    pub align: EditAlign,
    pub every_clip: bool,
}

impl EditHelperDialog {
    pub fn new(kind: u8, clip_id: ClipId) -> Self {
        Self { kind, clip_id, piece_frames: 2, on: 1, off: 1, ripple: kind == 1, frames: 3, keep: LengthKeep::Start, align: EditAlign::Centre, every_clip: false }
    }

    pub fn title(&self) -> &'static str {
        match self.kind {
            0 => "Strobe",
            1 => "Set Length",
            _ => "Copy Over Every Edit",
        }
    }

    /// The op the dialog asks for: a strobe between the sequence's In and
    /// Out when both lie inside the clip, else over the whole clip; the
    /// length of the selected clips, or of this one alone; copies over the
    /// cuts between the In and Out when they are set.
    pub fn op(&self, seq: &Sequence, selected: &[ClipId]) -> Op {
        let clip_id = ClipRef::Id(self.clip_id);
        let marks = match (seq.in_point, seq.out_point) {
            (Some(a), Some(b)) if b > a => Some((a, b)),
            _ => None,
        };
        let secs = |t: klipp_core::Timecode| Anchor::Secs(t.to_secs_f64());
        match self.kind {
            0 => {
                let inside = seq.all_clips().find(|c| c.id == self.clip_id).and_then(|c| marks.filter(|(a, b)| *a >= c.timeline_in && *b <= c.timeline_out));
                Op::Strobe { clip_id, piece_frames: self.piece_frames, from: inside.map(|(a, _)| secs(a)), to: inside.map(|(_, b)| secs(b)), on: self.on, off: self.off, ripple: self.ripple }
            }
            1 => {
                let mut clips: Vec<ClipRef> = selected.iter().map(|id| ClipRef::Id(*id)).collect();
                if !selected.contains(&self.clip_id) {
                    clips = vec![clip_id];
                }
                Op::SetLength { clips, frames: self.frames, keep: self.keep, ripple: self.ripple }
            }
            _ => Op::OverEdits { clip_id, tracks: Vec::new(), from: marks.map(|(a, _)| secs(a)), to: marks.map(|(_, b)| secs(b)), align: self.align, every_clip: self.every_clip },
        }
    }
}

/// What Match Clip Below does to `clip_id` (#2236): it starts where the
/// clip under it starts and is as long, as one undo step through the move
/// and set-length ops, which refuse what would run over a neighbour or
/// past its source. `None` with no clip under it, or when it matches
/// already.
pub fn match_clip_below(seq: &Sequence, clip_id: ClipId) -> Option<Vec<Op>> {
    klipp_document::matched::match_ops(seq, clip_id, klipp_document::matched::clip_below(seq, clip_id)?)
}

#[cfg(test)]
mod tests {
    /// Kept matched (#2236): a shot trimmed and recorded takes the clip
    /// matched to it along in the same step, and one undo puts both back.
    #[test]
    fn a_recorded_trim_takes_the_matched_clip_along_in_one_step() {
        use klipp_document::history::EditCommand;
        let mut state = crate::state::AppState::default();
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        let seq = &mut state.sequences[0];
        seq.tracks.insert(0, klipp_timeline::Track::new("V2", klipp_timeline::TrackKind::Video));
        let v: Vec<usize> = seq.tracks.iter().enumerate().filter(|(_, t)| t.kind == klipp_timeline::TrackKind::Video).map(|(i, _)| i).collect();
        let shot = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0));
        let mut title = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0));
        title.matched_to = Some(shot.id);
        let (shot_id, title_id) = (shot.id, title.id);
        seq.tracks[v[1]].clips = vec![shot.clone()];
        seq.tracks[v[0]].clips = vec![title];
        let out = |s: &crate::state::AppState, id| s.sequence().all_clips().find(|c| c.id == id).unwrap().timeline_out.to_secs_f64();

        let mut trimmed = shot.clone();
        trimmed.timeline_out = tc(3.0);
        trimmed.source_out = tc(3.0);
        let seq = &mut state.sequences[0];
        *seq.all_clips_mut().find(|c| c.id == shot_id).unwrap() = trimmed.clone();
        state.push_history(EditCommand::ReplaceClip { description: "Trim", old: Box::new(shot), new: Box::new(trimmed) });
        assert_eq!((out(&state, shot_id), out(&state, title_id)), (3.0, 3.0), "the title followed its shot");
        assert!(state.matched_follow_sync, "and the engine is told");

        let entry = state.take_undo().expect("one step");
        let (mut pool, mut fps) = (state.media_pool.clone(), state.fps);
        klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_inverse(&entry.command);
        assert_eq!((out(&state, shot_id), out(&state, title_id)), (4.0, 4.0), "one undo, both back");
    }

    /// Match Clip Below (#2236): a title over a shot takes the shot's
    /// start and length; with nothing under it, or already matching,
    /// there is nothing to do.
    #[test]
    fn a_clip_is_matched_to_the_clip_under_it() {
        let mut seq = Sequence::new_default();
        let rate = seq.fps.rate();
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, rate);
        let v = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        let shot = klipp_timeline::Clip::gap(tc(2.0), tc(6.0));
        seq.tracks[v].clips.push(shot);
        seq.tracks.insert(v, klipp_timeline::Track::new("Text 1", klipp_timeline::TrackKind::Text));
        let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Hello".into(), ..Default::default() });
        let title = klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Title(td), ..klipp_timeline::Clip::gap(tc(3.0), tc(4.0)) };
        let title_id = title.id;
        seq.tracks[v].clips.push(title);
        let ops = match_clip_below(&seq, title_id).expect("a shot under it");
        let frames = (4.0 * seq.fps.as_f64()).round() as i64;
        assert!(matches!(&ops[0], Op::Move { to: Anchor::Secs(s), .. } if (*s - 2.0).abs() < 1e-6), "moved first, as it grows: {ops:?}");
        assert!(matches!(&ops[1], Op::SetLength { frames: f, keep: LengthKeep::Start, ripple: false, .. } if *f == frames), "{ops:?}");
        // And the document takes them: the title ends up on the shot.
        let pool = klipp_media::MediaPool::new();
        let plan = klipp_document::ops::resolve(&seq, &pool, &ops).unwrap_or_else(|r| panic!("refused: {}", r.why));
        let mut seqs = vec![seq.clone()];
        let (mut p, mut fps) = (pool.clone(), seq.fps);
        let mut view = klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        for step in &plan.steps {
            view.apply_forward(&step.edit);
        }
        let placed = seqs[0].tracks.iter().flat_map(|t| &t.clips).find(|c| c.id == title_id).unwrap();
        assert_eq!((placed.timeline_in, placed.timeline_out), (tc(2.0), tc(6.0)), "on the shot");
        // Already matching: nothing to do.
        seq.tracks[v].clips[0].timeline_in = tc(2.0);
        seq.tracks[v].clips[0].timeline_out = tc(6.0);
        assert!(match_clip_below(&seq, title_id).is_none());
        // Nothing under it.
        seq.tracks[v].clips[0].timeline_in = tc(10.0);
        seq.tracks[v].clips[0].timeline_out = tc(12.0);
        assert!(match_clip_below(&seq, title_id).is_none());
    }

    use super::*;
    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::{Clip, Track, TrackKind};

    #[test]
    fn the_dialog_asks_for_the_op_its_settings_say() {
        let tc = |s: f64| RationalTime::from_secs_f64(s, 25);
        let mut seq = Sequence::new_default();
        let mut v = Track::new("V1", TrackKind::Video);
        let (a, b) = (Clip::media(MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0)), Clip::media(MediaId::new(), tc(4.0), tc(8.0), tc(0.0), tc(4.0)));
        let (ia, ib) = (a.id, b.id);
        v.clips = vec![a, b];
        seq.tracks = vec![v];
        // A strobe of the whole clip, then of the In to Out inside it.
        let strobe = EditHelperDialog::new(0, ia);
        assert!(matches!(strobe.op(&seq, &[]), Op::Strobe { from: None, to: None, piece_frames: 2, .. }));
        seq.in_point = Some(tc(1.0));
        seq.out_point = Some(tc(2.0));
        assert!(matches!(strobe.op(&seq, &[]), Op::Strobe { from: Some(Anchor::Secs(f)), to: Some(Anchor::Secs(t)), .. } if f == 1.0 && t == 2.0));
        // Set length: the selection when the clip is in it, else the clip.
        let set = EditHelperDialog::new(1, ia);
        assert!(matches!(set.op(&seq, &[ia, ib]), Op::SetLength { ref clips, frames: 3, ripple: true, .. } if clips.len() == 2));
        assert!(matches!(set.op(&seq, &[ib]), Op::SetLength { ref clips, .. } if *clips == vec![ClipRef::Id(ia)]));
        // Over edits: within the In and Out.
        assert!(matches!(EditHelperDialog::new(2, ia).op(&seq, &[]), Op::OverEdits { from: Some(_), to: Some(_), .. }));
    }
}
