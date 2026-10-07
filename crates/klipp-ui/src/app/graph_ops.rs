//! A batch of graph ops on one picture, as one undo step or nothing
//! (node-graph M2 step 15d): what the control socket's `graph_ops` and the
//! assistant send (`AppCommand::GraphOps`). A clip's picture goes through
//! `Clip::apply_picture_ops`; the other pictures (a track's, a grade
//! group's stages, the timeline grade) follow.

use super::*;
use klipp_graph_format::path::Root;
use klipp_graph_lift::ops::GraphOp;

impl KlippApp {
    pub(super) fn handle_graph_ops_commands(&mut self, cmd: &AppCommand) -> bool {
        let AppCommand::GraphOps { ops, label } = cmd else { return false };
        if let Err((at, why)) = self.apply_graph_ops(ops, label) {
            tracing::warn!(?at, %why, "graph ops refused");
            let location = at.map(|i| format!("op {i}")).unwrap_or_else(|| "batch".into());
            self.state.toast.show(format!("{label}: {location} refused: {why}"));
        }
        true
    }

    /// `ops` applied as one undo step named `label`, or nothing: the index
    /// of the op refused, and why. Applied, the node ids the picture holds
    /// under other ids once written (#1256).
    pub(super) fn apply_graph_ops(&mut self, ops: &[GraphOp], label: &str) -> Result<Vec<(klipp_graph_format::NodeId, klipp_graph_format::NodeId)>, (Option<usize>, String)> {
        let (edit, renamed) = graph_ops_edit(&self.state.sequences[self.state.active_sequence_idx], ops)?;
        self.apply_edits_as_one(&[edit], label);
        Ok(renamed)
    }
}

/// The history entry `ops` make on `seq`, built on a copy: nothing changes
/// yet, and the ids it renames ([`klipp_timeline::Clip::apply_picture_ops_renamed`]).
/// A refusal is the index of the op refused, and why.
fn graph_ops_edit(seq: &klipp_timeline::Sequence, ops: &[GraphOp]) -> Result<(EditCommand, Vec<(klipp_graph_format::NodeId, klipp_graph_format::NodeId)>), (Option<usize>, String)> {
    let root = ops.first().ok_or((None, "an empty batch has no edit".to_string()))?.path().root;
    if let Some(at) = ops.iter().position(|op| op.path().root != root) {
        return Err((Some(at), "a batch edits one picture".into()));
    }
    let Root::Clip { clip } = root else {
        return Err((Some(0), "only a clip's picture takes graph ops so far; a track's, a grade group's and the timeline's follow".into()));
    };
    let clip_id = klipp_core::ClipId(clip);
    let original = seq.tracks.iter().flat_map(|t| &t.clips).find(|c| c.id == clip_id).ok_or_else(|| (None, format!("no clip {clip} on this sequence's tracks")))?;
    let mut scratch = original.clone();
    let (edit, renamed) = scratch.apply_picture_ops_renamed(ops)?;
    Ok((EditCommand::PictureEdit { description: "Graph Edit", clip_id, edit: Box::new(edit) }, renamed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_graph_format::path::NodePath;
    use klipp_graph_format::{types, ParamValue};

    fn sequence() -> (klipp_timeline::Sequence, klipp_core::ClipId, Vec<klipp_graph_format::NodeId>) {
        let mut seq = klipp_timeline::Sequence::new_default();
        let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
        clip.push_effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 3.0 });
        clip.push_effect(klipp_effects::BuiltinEffect::Sharpen { amount: 0.5 });
        let fx = clip.picture_v2().unwrap().picture.graph.nodes.iter().filter(|n| n.type_id.as_str() == types::LEGACY_EFFECT).map(|n| n.id).collect();
        let id = clip.id;
        seq.tracks[0].clips.push(clip);
        (seq, id, fx)
    }

    /// A batch on a clip is one picture edit of that clip; the sequence is
    /// left as it was until the edit is applied.
    #[test]
    fn a_batch_on_a_clip_is_one_picture_edit() {
        let (seq, clip, fx) = sequence();
        let at = |id| NodePath::new(Root::Clip { clip: clip.0 }, [id]);
        let ops = [
            GraphOp::SetParam { path: at(fx[0]), key: "radius".into(), value: ParamValue::Float(5.0) },
            GraphOp::SetEnabled { path: at(fx[1]), enabled: false },
        ];
        let before = format!("{:?}", seq.tracks[0].clips[0].builtin_effects());
        let (edit, _) = graph_ops_edit(&seq, &ops).unwrap();
        let EditCommand::PictureEdit { clip_id, edit, .. } = edit else { panic!("a picture edit") };
        assert_eq!(clip_id, clip);
        let mut after = seq.tracks[0].clips[0].clone();
        after.restore_picture(&edit.after);
        assert!(matches!(after.builtin_effects()[0], klipp_effects::BuiltinEffect::GaussianBlur { radius } if radius == 5.0));
        assert_eq!(after.builtin_effects_enabled().get(1), Some(&false));
        assert_eq!(format!("{:?}", seq.tracks[0].clips[0].builtin_effects()), before, "built on a copy");
    }

    /// An effect added to a clip whose fields hold its effects comes back
    /// renamed (#1256): the edit's clip holds it under the renamed id, and
    /// the effect after it, moved down a slot, is renamed too.
    #[test]
    fn an_added_effect_comes_back_renamed() {
        let (seq, clip, fx) = sequence();
        let at = |id| NodePath::new(Root::Clip { clip: clip.0 }, [id]);
        let added = uuid::Uuid::from_u128(0xd1);
        let node = klipp_graph_format::Node {
            id: added,
            type_id: types::LEGACY_EFFECT.into(),
            type_version: 1,
            label: String::new(),
            enabled: true,
            params: Default::default(),
            anim: Default::default(),
            mix: None,
            wireless: Default::default(),
            group: None,
            legacy: Some(klipp_graph_format::Legacy { slot: None, payload: serde_json::to_value(klipp_effects::BuiltinEffect::Gamma { gamma: 1.3 }).unwrap(), keyframes: None, modifiers: Default::default() }),
            legacy_flags: None,
            origin: None,
        };
        let (edit, renamed) = graph_ops_edit(&seq, &[GraphOp::AddNode { after: at(fx[0]), node: Box::new(node) }]).unwrap();
        let EditCommand::PictureEdit { edit, .. } = edit else { panic!("a picture edit") };
        let mut after = seq.tracks[0].clips[0].clone();
        after.restore_picture(&edit.after);
        let now: Vec<_> = after.picture_v2().unwrap().picture.graph.nodes.iter().filter(|n| n.type_id.as_str() == types::LEGACY_EFFECT).map(|n| n.id).collect();
        assert_eq!(renamed.iter().find(|(was, _)| *was == added).map(|(_, now)| *now), Some(now[1]), "{renamed:?}");
        assert_eq!(renamed.iter().find(|(was, _)| *was == fx[1]).map(|(_, now)| *now), Some(now[2]), "{renamed:?}");
        assert!(!now.contains(&added));
    }

    /// A refusal names the op it stopped at: the first that couldn't be
    /// applied, another picture's op, or a picture that doesn't take ops yet.
    #[test]
    fn a_refusal_names_the_op_it_stopped_at() {
        let (seq, clip, fx) = sequence();
        let at = |id| NodePath::new(Root::Clip { clip: clip.0 }, [id]);
        let good = GraphOp::SetEnabled { path: at(fx[0]), enabled: false };
        let bad = GraphOp::SetParam { path: at(fx[0]), key: "radius".into(), value: ParamValue::Text("wide".into()) };
        let (i, why) = graph_ops_edit(&seq, &[good.clone(), good.clone(), bad]).unwrap_err();
        assert_eq!(i, Some(2), "{why}");
        let elsewhere = GraphOp::SetEnabled { path: NodePath::new(Root::Clip { clip: uuid::Uuid::nil() }, [fx[0]]), enabled: true };
        assert_eq!(graph_ops_edit(&seq, &[good.clone(), elsewhere]).unwrap_err().0, Some(1));
        let track = GraphOp::SetEnabled { path: NodePath::new(Root::TimelineGrade { sequence: uuid::Uuid::nil() }, [fx[0]]), enabled: true };
        assert!(graph_ops_edit(&seq, &[track]).unwrap_err().1.contains("only a clip's picture"));
        assert!(graph_ops_edit(&seq, &[]).is_err());
    }
}
