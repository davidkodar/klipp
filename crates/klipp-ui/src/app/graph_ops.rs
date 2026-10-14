//! A batch of graph ops on one picture, as one undo step or nothing
//! (node-graph M2 step 15d): what the control socket's `graph_ops` and the
//! assistant send (`AppCommand::GraphOps`). A clip's picture goes through
//! `Clip::apply_picture_ops`; the sequence's own grades (the timeline
//! grade, a grade group's stages, a track's grade) through
//! `Sequence::apply_root_grade_ops`, and a track's effect list through
//! `Sequence::apply_track_effect_ops` (#1214).

use super::*;
use klipp_graph_format::path::Root;
use klipp_graph_lift::ops::{GraphEdit, GraphOp};
use klipp_graph_lift::edit_planner::{Ungated, ungated_consumers};

#[derive(Debug)]
pub(super) struct GraphOpsOutcome {
    pub edit: EditCommand,
    pub renamed: Vec<(klipp_graph_format::NodeId, klipp_graph_format::NodeId)>,
    pub ungated: Vec<Ungated>,
}

impl AppState {
    pub(super) fn show_ungated(&mut self, nodes: &[Ungated]) {
        if !nodes.is_empty() {
            let noun = if nodes.len() == 1 { "node" } else { "nodes" };
            self.toast.show(format!("{} {noun} lost their mask: {}", nodes.len(), nodes.iter().map(|n| n.label.as_str()).collect::<Vec<_>>().join(", ")));
        }
    }
}

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
    /// under other ids once written (#1256), and readers that lost a mask.
    pub(super) fn apply_graph_ops(&mut self, ops: &[GraphOp], label: &str) -> Result<GraphOpsOutcome, (Option<usize>, String)> {
        let outcome = graph_ops_edit(&self.state.sequences[self.state.active_sequence_idx], ops)?;
        self.apply_edits_as_one(std::slice::from_ref(&outcome.edit), label);
        // A batch that adds nodes can put them outside the view: framed
        // again, once for the batch, unless the view was moved by hand
        // (#1905).
        if ops.iter().any(|op| matches!(op, GraphOp::AddNode { .. } | GraphOp::AddNodeBeside { .. })) {
            self.node_graph_v2_panel.refit_if_untouched();
        }
        self.state.show_ungated(&outcome.ungated);
        Ok(outcome)
    }
}

/// The history entry `ops` make on `seq`, built on a copy: nothing changes
/// yet, and the ids it renames ([`klipp_timeline::Clip::apply_picture_ops_renamed`]).
/// A refusal is the index of the op refused, and why.
pub(super) fn graph_ops_edit(seq: &klipp_timeline::Sequence, ops: &[GraphOp]) -> Result<GraphOpsOutcome, (Option<usize>, String)> {
    let root = ops.first().ok_or((None, "an empty batch has no edit".to_string()))?.path().root;
    if let Some(at) = ops.iter().position(|op| op.path().root != root) {
        return Err((Some(at), "a batch edits one picture".into()));
    }
    let clip = match root {
        Root::Clip { clip } => clip,
        // A track's effect list, through its own list (#1214).
        Root::Track { track, part: klipp_graph_format::path::TrackPart::Effects } => {
            let t = seq.tracks.iter().find(|t| t.id.0 == track).ok_or_else(|| (None, format!("no track {track} on this sequence")))?;
            // Its effects are named by place once written: the reply names
            // each by the id it then has (#1911).
            let (_, effects, renamed) = seq.clone().apply_track_effect_ops(ops).map_err(|e| (None, e))?;
            let old = t.settings();
            let new = klipp_timeline::TrackSettings { effects, ..old.clone() };
            return Ok(GraphOpsOutcome { edit: EditCommand::SetTrackSettings { changes: vec![(t.id, old, new)] }, renamed, ungated: Vec::new() });
        }
        // The sequence's own grades, a track's among them, as the Colour
        // workspace edits them (#1214): whole, as one grade step. One the
        // sequence doesn't have yet has no node a batch could name, and is
        // refused.
        _ => {
            let (old, new) = seq.clone().apply_root_grade_ops(ops, None).map_err(|e| (None, e))?;
            let edit = EditCommand::SetRootGrade { description: "Graph Edit", root, old: old.map(Box::new), new: Some(Box::new(new)) };
            return Ok(GraphOpsOutcome { edit, renamed: Vec::new(), ungated: Vec::new() });
        }
    };
    let clip_id = klipp_core::ClipId(clip);
    let original = seq.tracks.iter().flat_map(|t| &t.clips).find(|c| c.id == clip_id).ok_or_else(|| (None, format!("no clip {clip} on this sequence's tracks")))?;
    let mut scratch = original.clone();
    let (edit, renamed) = scratch.apply_picture_ops_renamed(ops)?;
    let ungated = if ops.iter().any(|op| matches!(op, GraphOp::RemoveNode { .. })) {
        let picture = |c: &klipp_timeline::Clip| {
            let p = c.picture_v2().map_err(|e| (None, e.to_string()))?;
            serde_json::to_value(&p.picture).map_err(|e| (None, e.to_string()))
        };
        ungated_consumers(&GraphEdit { root, before: picture(original)?, after: picture(&scratch)? }, &klipp_timeline::RegistryCatalog)
            .map_err(|e| (None, e))?
    } else { Vec::new() };
    Ok(GraphOpsOutcome { edit: EditCommand::PictureEdit { description: "Graph Edit", clip_id, edit: Box::new(edit) }, renamed, ungated })
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_graph_format::path::NodePath;
    use klipp_graph_format::{types, ParamValue};
    use klipp_timeline::test_clip::{self, TestClip};

    fn sequence() -> (klipp_timeline::Sequence, klipp_core::ClipId, Vec<klipp_graph_format::NodeId>) {
        let mut seq = klipp_timeline::Sequence::new_default();
        let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        let clip = TestClip::of(klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0)))
            .effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 3.0 })
            .effect(klipp_effects::BuiltinEffect::Sharpen { amount: 0.5 })
            .build();
        let fx = clip.picture_v2().unwrap().picture.graph.nodes.iter().filter(|n| n.type_id.as_str() == types::LEGACY_EFFECT).map(|n| n.id).collect();
        let id = clip.id;
        seq.tracks[0].clips.push(clip);
        (seq, id, fx)
    }

    /// A batch on each of the sequence's own grades (the timeline grade,
    /// a grade group's stage, a track's grade) is one grade step of it
    /// (#1214): the grade before and after, the edit in the after, the
    /// sequence left as it was until it's applied.
    #[test]
    fn a_batch_on_a_sequence_grade_is_one_grade_step() {
        use klipp_graph_format::lifted::Stage;
        use klipp_graph_format::path::TrackPart;
        let (mut seq, ..) = sequence();
        let group = klipp_timeline::GradeGroup::new("Night");
        let gid = group.id.0;
        seq.grade_groups.push(group);
        let tid = seq.tracks.iter().find(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap().id.0;
        for root in [Root::TimelineGrade { sequence: seq.id.0 }, Root::GroupStage { group: gid, stage: Stage::Post }, Root::Track { track: tid, part: TrackPart::Grade }] {
            let grade = klipp_nodes::NodeGraph::default();
            let corrector = grade.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).unwrap().id;
            seq.set_root_grade(root, Some(grade.clone())).unwrap();
            let op = GraphOp::SetParam { path: NodePath::new(root, [corrector]), key: "exposure".into(), value: ParamValue::Float(0.7) };
            let GraphOpsOutcome { edit, .. } = graph_ops_edit(&seq, &[op]).unwrap_or_else(|e| panic!("{root:?}: {e:?}"));
            let EditCommand::SetRootGrade { root: r, old, new, .. } = edit else { panic!("{root:?}: a grade step") };
            assert_eq!(r, root);
            assert_eq!(old.as_deref(), Some(&grade), "{root:?}: the grade before");
            let exposure = new.as_ref().and_then(|g| g.nodes.iter().find(|n| n.id == corrector)).and_then(|n| n.params.exposure);
            assert_eq!(exposure, Some(0.7), "{root:?}: the edit in the grade after");
            assert_eq!(seq.root_grade(root).unwrap(), Some(&grade), "{root:?}: built on a copy");
        }
    }

    /// A batch on a track's effect list is one track step (#1214): its
    /// effects before and after, through the track's own list; a key op is
    /// refused, as a track's effects keep no keyframes.
    #[test]
    fn a_batch_on_a_track_s_effects_is_one_track_step() {
        use klipp_graph_format::path::TrackPart;
        let (mut seq, ..) = sequence();
        let at = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        // The track's list as it's saved.
        seq.tracks[at].effects = serde_json::from_value(serde_json::json!({ "video": [{ "type": "gaussian_blur", "radius": 3.0 }], "video_enabled": [true] })).unwrap();
        let radius = |fx: &klipp_timeline::TrackEffects| serde_json::to_value(fx).unwrap()["video"][0]["radius"].as_f64();
        let tid = seq.tracks[at].id;
        let root = Root::Track { track: tid.0, part: TrackPart::Effects };
        let node = seq.lift_track_parts(tid.0).unwrap().0[0].id;
        let path = NodePath::new(root, [node]);
        let GraphOpsOutcome { edit, .. } = graph_ops_edit(&seq, &[GraphOp::SetParam { path: path.clone(), key: "radius".into(), value: ParamValue::Float(6.0) }]).unwrap();
        let EditCommand::SetTrackSettings { changes } = edit else { panic!("a track step") };
        let [(t, old, new)] = changes.as_slice() else { panic!("one track") };
        assert_eq!(*t, tid);
        assert_eq!(old, &seq.tracks[at].settings(), "the track before");
        assert_eq!(radius(&new.effects), Some(6.0), "the edit in the track's list");
        assert_eq!((new.opacity, new.volume, new.pan), (old.opacity, old.volume, old.pan), "only its effects");
        assert_eq!(radius(&seq.tracks[at].effects), Some(3.0), "built on a copy");
        let key = serde_json::json!({ "time_secs": 0.5, "value": 2.0, "interpolation": "Linear", "tangent_in": [0.0, 0.0], "tangent_out": [0.0, 0.0] });
        let refused = graph_ops_edit(&seq, &[GraphOp::AddKeyAt { path, key: "radius".into(), keyframe: key }]).unwrap_err();
        assert!(refused.1.contains("no keyframes"), "{refused:?}");
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
        let before = format!("{:?}", test_clip::effects(&seq.tracks[0].clips[0]));
        let GraphOpsOutcome { edit, .. } = graph_ops_edit(&seq, &ops).unwrap();
        let EditCommand::PictureEdit { clip_id, edit, .. } = edit else { panic!("a picture edit") };
        assert_eq!(clip_id, clip);
        let mut after = seq.tracks[0].clips[0].clone();
        after.restore_picture(&edit.after);
        assert!(matches!(test_clip::effects(&after)[0], klipp_effects::BuiltinEffect::GaussianBlur { radius } if radius == 5.0));
        assert_eq!(test_clip::enabled(&after).get(1), Some(&false));
        assert_eq!(format!("{:?}", test_clip::effects(&seq.tracks[0].clips[0])), before, "built on a copy");
    }

    /// A Gamma effect node under `id`, as an add sends it.
    fn gamma(id: uuid::Uuid) -> klipp_graph_format::Node {
        klipp_graph_format::Node {
            id,
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
        }
    }

    /// An effect added to a clip whose fields hold its effects keeps its id
    /// (effects' node ids are their identity, M3 proposal 6): the edit's
    /// clip holds it under the batch's id, the effect after it keeps its own
    /// though it moved down a slot, and the reply renames nothing.
    #[test]
    fn an_added_effect_keeps_its_id() {
        let (seq, clip, fx) = sequence();
        let at = |id| NodePath::new(Root::Clip { clip: clip.0 }, [id]);
        let added = uuid::Uuid::from_u128(0xd1);
        let node = gamma(added);
        let GraphOpsOutcome { edit, renamed, .. } = graph_ops_edit(&seq, &[GraphOp::AddNode { after: at(fx[0]), node: Box::new(node) }]).unwrap();
        let EditCommand::PictureEdit { edit, .. } = edit else { panic!("a picture edit") };
        let mut after = seq.tracks[0].clips[0].clone();
        after.restore_picture(&edit.after);
        let now: Vec<_> = after.picture_v2().unwrap().picture.graph.nodes.iter().filter(|n| n.type_id.as_str() == types::LEGACY_EFFECT).map(|n| n.id).collect();
        assert!(renamed.is_empty(), "{renamed:?}");
        assert_eq!(now, vec![fx[0], added, fx[1]], "each effect under its own id");
    }

    /// `edit` applied to `seqs` as the app's history does: forward, or undone.
    fn applied(seqs: &mut Vec<klipp_timeline::Sequence>, edit: &EditCommand, undo: bool) {
        let mut pool = klipp_media::MediaPool::new();
        let mut fps = seqs[0].fps;
        let mut view = klipp_document::apply::DocumentView { sequences: seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        if undo {
            view.apply_inverse(edit);
        } else {
            view.apply_forward(edit);
        }
    }

    /// `seqs` saved as a project and opened again.
    fn reopened(seqs: &[klipp_timeline::Sequence], name: &str) -> klipp_timeline::Sequence {
        let dir = std::env::temp_dir().join(format!("klipp_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        klipp_project::save_project(&dir, seqs, &klipp_media::MediaPool::new()).unwrap();
        let (mut sequences, ..) = klipp_project::load_project(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        sequences.remove(0)
    }

    /// A sequence with a timeline grade, a grade group's pre stage, a
    /// grade on its video track and two effects in that track's list; the
    /// track's id.
    fn sequence_with_roots() -> (klipp_timeline::Sequence, uuid::Uuid) {
        use klipp_graph_format::lifted::Stage;
        use klipp_graph_format::path::TrackPart;
        let (mut seq, ..) = sequence();
        let group = klipp_timeline::GradeGroup::new("Night");
        let gid = group.id.0;
        seq.grade_groups.push(group);
        let at = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        let tid = seq.tracks[at].id.0;
        seq.tracks[at].effects = serde_json::from_value(serde_json::json!({
            "video": [{ "type": "gaussian_blur", "radius": 3.0 }, { "type": "sharpen", "amount": 0.5 }],
            "video_enabled": [true, true],
        })).unwrap();
        for root in [Root::TimelineGrade { sequence: seq.id.0 }, Root::GroupStage { group: gid, stage: Stage::Pre }, Root::Track { track: tid, part: TrackPart::Grade }] {
            seq.set_root_grade(root, Some(klipp_nodes::NodeGraph::default())).unwrap();
        }
        (seq, tid)
    }

    /// The value a socket batch on `root` sets in the tests below: the
    /// Corrector's exposure in a grade, the first effect's radius in a
    /// track's list, as the saved sequence holds it.
    fn written(seq: &klipp_timeline::Sequence, root: Root) -> Option<f64> {
        use klipp_graph_format::path::TrackPart;
        match root {
            Root::Track { track, part: TrackPart::Effects } => {
                let t = seq.tracks.iter().find(|t| t.id.0 == track)?;
                serde_json::to_value(&t.effects).ok()?["video"][0]["radius"].as_f64()
            }
            _ => seq.root_grade(root).ok()??.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector)?.params.exposure.map(f64::from),
        }
    }

    /// #1911: a socket batch on each of the sequence's own pictures (the
    /// timeline grade, a grade group's stage, a track's grade and its
    /// effects) is one step: undone, the sequence is as it was; redone, as
    /// the batch left it; and its edit comes back with the project.
    #[test]
    fn a_batch_on_each_sequence_root_undoes_redoes_and_saves() {
        use klipp_graph_format::lifted::Stage;
        use klipp_graph_format::path::TrackPart;
        let (seq, tid) = sequence_with_roots();
        let gid = seq.grade_groups[0].id.0;
        let roots = [
            Root::TimelineGrade { sequence: seq.id.0 },
            Root::GroupStage { group: gid, stage: Stage::Pre },
            Root::Track { track: tid, part: TrackPart::Grade },
            Root::Track { track: tid, part: TrackPart::Effects },
        ];
        let snapshot = |s: &klipp_timeline::Sequence| serde_json::to_value(s).unwrap();
        for root in roots {
            let mut seqs = vec![seq.clone()];
            let (op, want) = match root {
                Root::Track { part: TrackPart::Effects, .. } => {
                    let node = seq.lift_track_parts(tid).unwrap().0[0].id;
                    (GraphOp::SetParam { path: NodePath::new(root, [node]), key: "radius".into(), value: ParamValue::Float(6.0) }, 6.0)
                }
                _ => {
                    let corrector = seq.root_grade(root).unwrap().unwrap().nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).unwrap().id;
                    (GraphOp::SetParam { path: NodePath::new(root, [corrector]), key: "exposure".into(), value: ParamValue::Float(0.75) }, 0.75)
                }
            };
            let before = snapshot(&seqs[0]);
            let GraphOpsOutcome { edit, .. } = graph_ops_edit(&seqs[0], &[op]).unwrap_or_else(|e| panic!("{root:?}: {e:?}"));
            applied(&mut seqs, &edit, false);
            assert_eq!(written(&seqs[0], root), Some(want), "{root:?}: the batch's edit");
            let after = snapshot(&seqs[0]);
            applied(&mut seqs, &edit, true);
            assert_eq!(snapshot(&seqs[0]), before, "{root:?}: one undo restores the sequence");
            applied(&mut seqs, &edit, false);
            assert_eq!(snapshot(&seqs[0]), after, "{root:?}: redo brings the edit back");
            assert_eq!(written(&reopened(&seqs, "graph_root_save"), root), Some(want), "{root:?}: the edit comes back with the project");
        }
    }

    /// #1911: an effect added to a track's list, which keeps no effect
    /// ids, is answered with the id the written list names it by, and the
    /// effect it pushed down a slot with its new one: each id the reply
    /// gives leads to its effect.
    #[test]
    fn an_effect_added_to_a_track_is_answered_with_its_written_id() {
        use klipp_graph_format::path::TrackPart;
        let (seq, tid) = sequence_with_roots();
        let root = Root::Track { track: tid, part: TrackPart::Effects };
        let fx: Vec<_> = seq.lift_track_parts(tid).unwrap().0.iter().map(|n| n.id).collect();
        let added = uuid::Uuid::from_u128(0xd1);
        let node = gamma(added);
        let GraphOpsOutcome { edit, renamed, .. } = graph_ops_edit(&seq, &[GraphOp::AddNode { after: NodePath::new(root, [fx[0]]), node: Box::new(node) }]).unwrap();
        let mut seqs = vec![seq.clone()];
        applied(&mut seqs, &edit, false);
        let now = seqs[0].lift_track_parts(tid).unwrap().0;
        let named = |id: uuid::Uuid| renamed.iter().find(|(was, _)| *was == id).map_or(id, |(_, now)| *now);
        let tag = |id: uuid::Uuid| now.iter().find(|n| n.id == named(id)).and_then(|n| klipp_graph_lift::effect_params::tag(n).map(str::to_owned));
        assert_eq!(now.len(), 3);
        assert_eq!(tag(fx[0]).as_deref(), Some("gaussian_blur"), "{renamed:?}");
        assert_eq!(tag(added).as_deref(), Some("gamma"), "the added effect: {renamed:?}");
        assert_eq!(tag(fx[1]).as_deref(), Some("sharpen"), "the effect pushed down a slot: {renamed:?}");
        // Removed, the effects after it move up a slot and are named so.
        let first = now[0].id;
        let GraphOpsOutcome { edit, renamed, .. } = graph_ops_edit(&seqs[0], &[GraphOp::RemoveNode { path: NodePath::new(root, [first]) }]).unwrap();
        let ids: Vec<_> = now.iter().map(|n| n.id).collect();
        applied(&mut seqs, &edit, false);
        let now = seqs[0].lift_track_parts(tid).unwrap().0;
        let named = |id: uuid::Uuid| renamed.iter().find(|(was, _)| *was == id).map_or(id, |(_, now)| *now);
        let tag = |id: uuid::Uuid| now.iter().find(|n| n.id == named(id)).and_then(|n| klipp_graph_lift::effect_params::tag(n).map(str::to_owned));
        assert_eq!(now.len(), 2);
        assert_eq!((tag(ids[1]).as_deref(), tag(ids[2]).as_deref()), (Some("gamma"), Some("sharpen")), "{renamed:?}");
    }

    /// A refusal names the op it stopped at: the first that couldn't be
    /// applied, another picture's op, or a grade the sequence doesn't have.
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
        let none = GraphOp::SetEnabled { path: NodePath::new(Root::TimelineGrade { sequence: seq.id.0 }, [fx[0]]), enabled: true };
        assert!(graph_ops_edit(&seq, &[none]).unwrap_err().1.contains("doesn't exist yet"), "a grade the sequence doesn't have");
        assert!(graph_ops_edit(&seq, &[]).is_err());
    }
}
