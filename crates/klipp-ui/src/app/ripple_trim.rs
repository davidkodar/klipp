//! Trim Start and Trim End: a clip's edge brought to the playhead, what
//! follows closing up. A linked clip takes its partner along when Linked
//! Selection is on, as dragging the edge does, so sound and picture end
//! together.

use super::selection_removal::magnetic;
use crate::history::EditCommand;
use crate::state::AppState;
use klipp_core::ClipId;
use klipp_document::ops::{self, Edge};
use klipp_timeline::magnetic::MagneticEditor;

pub(super) enum RippleTrim {
    /// The playhead isn't inside the clip.
    Nothing,
    /// Made on the sequence, its undo step pushed.
    Made,
    /// Planned by the document, for the app to apply.
    Planned(Box<EditCommand>),
    Refused(String),
}

/// Trim `clip_id`'s `edge` to the playhead.
pub(super) fn to_playhead(state: &mut AppState, clip_id: ClipId, edge: Edge) -> RippleTrim {
    let playhead = state.playhead;
    let linked = state.linked_selection;
    let seq = state.sequence();
    let Some((ti, ci)) = seq.find_clip(clip_id) else { return RippleTrim::Nothing };
    let clip = &seq.tracks[ti].clips[ci];
    if playhead <= clip.timeline_in || playhead >= clip.timeline_out {
        return RippleTrim::Nothing;
    }
    let with_partner = linked && clip.linked_clip_id.is_some_and(|p| seq.find_clip(p).is_some());
    if magnetic(state, ti) && !with_partner {
        let seq = state.sequence_mut();
        let result = match edge {
            Edge::In => MagneticEditor::trim_head(seq, clip_id, playhead),
            Edge::Out => MagneticEditor::trim_tail(seq, clip_id, playhead),
        };
        state.push_history(EditCommand::Magnetic(result.command));
        return RippleTrim::Made;
    }
    match ops::trim_at_linked(state.sequence(), &state.media_pool, clip_id, edge, playhead, true, linked) {
        Ok(edit) => RippleTrim::Planned(Box::new(edit)),
        Err(r) => RippleTrim::Refused(r.why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::RationalTime;
    use klipp_document::apply::DocumentView;
    use klipp_timeline::Clip;

    fn tc(secs: f64) -> RationalTime {
        RationalTime::from_secs_f64(secs, 25)
    }

    /// Three linked picture and sound pairs, 10 s each, end to end on the
    /// default video and audio tracks: their ids, picture first.
    fn three_pairs() -> (AppState, Vec<(ClipId, ClipId)>) {
        let mut state = AppState::default();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/ripple-trim.mov".into(), None);
        item.duration = tc(60.0);
        let media = state.media_pool.add(item);
        let mut ids = Vec::new();
        for n in 0..3 {
            let at = n as f64 * 10.0;
            let mut v = Clip::media(media, tc(at), tc(at + 10.0), tc(at), tc(at + 10.0));
            let mut a = Clip::media(media, tc(at), tc(at + 10.0), tc(at), tc(at + 10.0));
            v.linked_clip_id = Some(a.id);
            a.linked_clip_id = Some(v.id);
            ids.push((v.id, a.id));
            state.sequences[0].tracks[0].clips.push(v);
            state.sequences[0].tracks[1].clips.push(a);
        }
        (state, ids)
    }

    /// The trim as the app makes it, the playhead at `at`.
    fn trim(state: &mut AppState, id: ClipId, edge: Edge, at: f64) {
        state.playhead = tc(at);
        match to_playhead(state, id, edge) {
            RippleTrim::Planned(edit) => {
                let active = state.active_sequence_idx;
                let mut view = DocumentView { sequences: &mut state.sequences, active, media_pool: &mut state.media_pool, fps: &mut state.fps };
                let _ = view.apply_forward(&edit);
                state.push_history(*edit);
            }
            RippleTrim::Refused(why) => panic!("refused: {why}"),
            RippleTrim::Made | RippleTrim::Nothing => {}
        }
    }

    /// A clip's timeline in and out and source in, seconds.
    fn span(state: &AppState, id: ClipId) -> (f64, f64, f64) {
        let c = state.sequence().all_clips().find(|c| c.id == id).unwrap();
        (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64(), c.source_in.to_secs_f64())
    }

    #[test]
    fn trim_end_takes_the_linked_partner_from_either_side() {
        for picture_selected in [true, false] {
            let (mut state, ids) = three_pairs();
            let (v, a) = ids[1];
            trim(&mut state, if picture_selected { v } else { a }, Edge::Out, 14.0);
            assert_eq!(span(&state, v).1, 14.0, "picture ends at the playhead");
            assert_eq!(span(&state, a).1, 14.0, "sound ends with it");
            let (later_v, later_a) = ids[2];
            assert_eq!(span(&state, later_v), span(&state, later_a), "what follows stays in sync");
            assert_eq!(span(&state, later_v).0, 14.0, "and closes up");
        }
    }

    #[test]
    fn trim_start_takes_the_linked_partner_from_either_side() {
        for picture_selected in [true, false] {
            let (mut state, ids) = three_pairs();
            let (v, a) = ids[1];
            trim(&mut state, if picture_selected { v } else { a }, Edge::In, 14.0);
            assert_eq!(span(&state, v), span(&state, a), "sound and picture start together, on the same source");
            assert_eq!(span(&state, v).2, 14.0, "the opening is gone");
            let (later_v, later_a) = ids[2];
            assert_eq!(span(&state, later_v), span(&state, later_a), "what follows stays in sync");
            assert_eq!(span(&state, later_v).0, span(&state, v).1, "and closes up");
        }
    }

    #[test]
    fn with_linked_selection_off_a_trim_leaves_the_partner() {
        let (mut state, ids) = three_pairs();
        state.linked_selection = false;
        let (v, a) = ids[1];
        trim(&mut state, v, Edge::Out, 14.0);
        assert_eq!(span(&state, v).1, 14.0);
        assert_eq!(span(&state, a), (10.0, 20.0, 10.0), "the sound stays whole");
    }

    #[test]
    fn a_clip_without_a_partner_still_trims_and_closes_up() {
        let (mut state, ids) = three_pairs();
        for (v, a) in &ids {
            for c in state.sequences[0].tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
                if c.id == *v || c.id == *a {
                    c.linked_clip_id = None;
                }
            }
        }
        let (v, a) = ids[1];
        trim(&mut state, v, Edge::Out, 14.0);
        assert_eq!(span(&state, v).1, 14.0);
        assert_eq!(span(&state, ids[2].0).0, 14.0, "the next picture closes up");
        assert_eq!(span(&state, a), (10.0, 20.0, 10.0), "the sound isn't touched");
    }

    #[test]
    fn a_playhead_outside_the_clip_trims_nothing() {
        let (mut state, ids) = three_pairs();
        let before = state.sequence().clone();
        for at in [10.0, 20.0, 25.0] {
            trim(&mut state, ids[1].0, Edge::Out, at);
        }
        assert_eq!(serde_json::to_string(state.sequence()).unwrap(), serde_json::to_string(&before).unwrap());
    }

    /// One undo step brings back both sides and everything that moved.
    #[test]
    fn one_undo_restores_both_sides() {
        let (mut state, ids) = three_pairs();
        let before = serde_json::to_string(state.sequence()).unwrap();
        trim(&mut state, ids[1].0, Edge::Out, 14.0);
        let entry = state.take_undo().expect("a step");
        let active = state.active_sequence_idx;
        let mut view = DocumentView { sequences: &mut state.sequences, active, media_pool: &mut state.media_pool, fps: &mut state.fps };
        let _ = view.apply_inverse(&entry.command);
        assert_eq!(serde_json::to_string(state.sequence()).unwrap(), before);
    }
}
