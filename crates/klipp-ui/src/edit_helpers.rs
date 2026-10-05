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

#[cfg(test)]
mod tests {
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
