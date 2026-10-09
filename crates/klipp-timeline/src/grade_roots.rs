//! A sequence's own grades edited by path (M3 step 19): the timeline grade
//! (`Root::TimelineGrade`) and a grade group's pre-clip and post-clip
//! stages (`Root::GroupStage`). Each is kept as a v1 grade graph; a path
//! edit lifts it as paths address it, applies the ops there and lowers it
//! back, so the stored form is the one the old route writes.
//!
//! A track's grade (`Root::Track`) has no inspector to edit it yet (#1408)
//! and is refused here. So are a correction's mix and gate mask channel on
//! any of these: only a clip's picture keeps them (#1321).

use crate::{GradeStage, Sequence};
use klipp_graph_format::lifted::Stage;
use klipp_graph_format::path::Root;
use klipp_graph_format::Graph;
use klipp_graph_lift::ops::GraphOp;
use klipp_nodes::NodeGraph;

fn stage_of(stage: Stage) -> GradeStage {
    match stage {
        Stage::Pre => GradeStage::Pre,
        Stage::Post => GradeStage::Post,
    }
}

/// Why a path edit of `root` is refused before it is tried, if it is.
fn refused(root: Root, op: &GraphOp) -> Option<String> {
    match root {
        Root::Track { .. } => Some("a track's grade isn't edited by path yet (#1408)".into()),
        Root::TimelineGrade { .. } | Root::GroupStage { .. } if matches!(op, GraphOp::SetMix { .. } | GraphOp::SetMaskChannel { .. }) => {
            Some("only a clip's grade keeps a correction's mix and mask channel so far (#1321)".into())
        }
        _ => None,
    }
}

impl Sequence {
    /// The grade `root` names, as it is stored: `None` when it has none
    /// (a sequence with no timeline grade, a group without that stage).
    /// `Err` for a root that isn't one of this sequence's own grades.
    pub fn root_grade(&self, root: Root) -> Result<Option<&NodeGraph>, String> {
        match root {
            Root::TimelineGrade { sequence } if sequence == self.id.0 => Ok(self.timeline_grade()),
            Root::GroupStage { group, stage } => {
                let g = self.grade_groups.iter().find(|g| g.id.0 == group).ok_or("no such grade group")?;
                Ok(g.stage(stage_of(stage)))
            }
            _ => Err("not one of this sequence's own grades".into()),
        }
    }

    /// `grade`, as `root` would hold it, lifted as its paths address it:
    /// its nodes at the top, `[node]`.
    pub fn lift_root_grade(&self, root: Root, grade: &NodeGraph) -> Result<Graph, String> {
        use klipp_graph_lift::{lift, splice, to_v2};
        let structural = match root {
            Root::TimelineGrade { .. } => {
                let raw = serde_json::json!({ "id": self.id.0, "timeline_graph": grade });
                to_v2::timeline_to_v2(raw.as_object().expect("an object")).map_err(|e| e.to_string())?
            }
            Root::GroupStage { group, stage } => {
                let mut g = self.grade_groups.iter().find(|g| g.id.0 == group).ok_or("no such grade group")?.clone();
                g.stage_slot(stage_of(stage)).set(Some(grade.clone()));
                let raw = serde_json::to_value(&g).map_err(|e| e.to_string())?;
                let (pre, post) = to_v2::stages_to_v2(raw.as_object().ok_or("a grade group must be an object")?).map_err(|e| e.to_string())?;
                match stage {
                    Stage::Pre => pre,
                    Stage::Post => post,
                }
            }
            _ => return Err("not one of this sequence's own grades".into()),
        };
        let structural = structural.ok_or("the grade lifts to nothing")?;
        match root {
            Root::GroupStage { .. } => splice::stage_body(&structural),
            _ => lift::lift_grade_only(&structural, false),
        }
    }

    /// Store `grade` as `root`'s, or take it off (`None`).
    pub fn set_root_grade(&mut self, root: Root, grade: Option<NodeGraph>) -> Result<(), String> {
        match root {
            Root::TimelineGrade { sequence } if sequence == self.id.0 => {
                self.timeline_grade_slot().set(grade);
                Ok(())
            }
            Root::GroupStage { group, stage } => {
                let g = self.grade_groups.iter_mut().find(|g| g.id.0 == group).ok_or("no such grade group")?;
                g.stage_slot(stage_of(stage)).set(grade);
                Ok(())
            }
            _ => Err("not one of this sequence's own grades".into()),
        }
    }

    /// Apply `ops`, all on one of this sequence's own grades, as one edit:
    /// the grade lifted (`start` standing for it when the root has none
    /// yet, the graph the inspector drew), the ops applied there with the
    /// registry's checks, and the result lowered and stored. The grade
    /// before (`None`: it had none) and after, for the edit's undo and
    /// redo. Nothing is stored when any op is refused.
    pub fn apply_root_grade_ops(&mut self, ops: &[GraphOp], start: Option<&NodeGraph>) -> Result<(Option<NodeGraph>, NodeGraph), String> {
        let root = ops.first().ok_or("an empty batch has no edit")?.path().root;
        if ops.iter().any(|op| op.path().root != root) {
            return Err("a batch edits one grade".into());
        }
        if let Some(why) = ops.iter().find_map(|op| refused(root, op)) {
            return Err(why);
        }
        let before = self.root_grade(root)?.cloned();
        let lifted = match before.as_ref().or(start) {
            Some(g) => self.lift_root_grade(root, g)?,
            None => return Err("this grade doesn't exist yet".into()),
        };
        let mut docs = klipp_graph_lift::paths::Documents::default();
        match root {
            Root::TimelineGrade { sequence } => {
                docs.timeline.insert(sequence, lifted);
            }
            Root::GroupStage { group, stage } => {
                docs.stages.insert((group, stage), lifted);
            }
            _ => unreachable!("refused above"),
        }
        klipp_graph_lift::ops::apply_batch_with_catalog(&mut docs, ops, &crate::RegistryCatalog)?;
        let edited = match root {
            Root::TimelineGrade { sequence } => docs.timeline.remove(&sequence),
            Root::GroupStage { group, stage } => docs.stages.remove(&(group, stage)),
            _ => None,
        }
        .ok_or("the edit lost its grade")?;
        let v1 = klipp_graph_lift::to_v1::grade_only(&klipp_graph_lift::delift::grade_only(&edited, uuid::Uuid::nil())?)?;
        let after: NodeGraph = serde_json::from_value(v1).map_err(|e| e.to_string())?;
        self.set_root_grade(root, Some(after.clone()))?;
        Ok((before, after))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GradeGroup;
    use klipp_graph_format::path::NodePath;
    use klipp_graph_format::ParamValue;

    fn corrector(g: &NodeGraph) -> klipp_nodes::NodeId {
        g.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).unwrap().id
    }

    fn set_exposure(root: Root, node: klipp_nodes::NodeId, v: f32) -> GraphOp {
        GraphOp::SetParam { path: NodePath::new(root, [node]), key: "exposure".into(), value: ParamValue::Float(v) }
    }

    /// A path edit of the timeline grade or a group stage lands in the
    /// grade stored there, as the old route stores it: the edited value on
    /// the same node, every node id kept, and the grade before handed back.
    #[test]
    fn a_path_edit_of_a_sequence_grade_lands_in_its_stored_grade() {
        let mut seq = Sequence::new_default();
        let mut group = GradeGroup::new("Night");
        group.stage_slot(GradeStage::Post).set(Some(NodeGraph::default()));
        let gid = group.id.0;
        seq.grade_groups.push(group);
        seq.timeline_grade_slot().set(Some(NodeGraph::default()));
        for root in [Root::TimelineGrade { sequence: seq.id.0 }, Root::GroupStage { group: gid, stage: Stage::Post }] {
            let before = seq.root_grade(root).unwrap().cloned().unwrap();
            let node = corrector(&before);
            let (old, new) = seq.apply_root_grade_ops(&[set_exposure(root, node, 0.75)], None).unwrap();
            assert_eq!(old.as_ref().map(|g| serde_json::to_value(g).unwrap()), Some(serde_json::to_value(&before).unwrap()), "{root:?}: the grade before");
            let stored = seq.root_grade(root).unwrap().unwrap();
            assert_eq!(serde_json::to_value(stored).unwrap(), serde_json::to_value(&new).unwrap(), "{root:?}: stored");
            assert_eq!(stored.nodes.iter().find(|n| n.id == node).unwrap().params.exposure, Some(0.75), "{root:?}");
            let ids = |g: &NodeGraph| g.nodes.iter().map(|n| n.id).collect::<Vec<_>>();
            assert_eq!(ids(stored), ids(&before), "{root:?}: every node id kept");
        }
    }

    /// A root with no grade yet takes its first edit on `start`, the graph
    /// the inspector drew; with no start, the edit is refused and nothing
    /// is stored.
    #[test]
    fn a_first_edit_begins_from_the_graph_drawn() {
        let mut seq = Sequence::new_default();
        let root = Root::TimelineGrade { sequence: seq.id.0 };
        let start = NodeGraph::default();
        let node = corrector(&start);
        assert!(seq.apply_root_grade_ops(&[set_exposure(root, node, 0.5)], None).is_err());
        assert!(seq.root_grade(root).unwrap().is_none(), "nothing stored");
        let (old, _) = seq.apply_root_grade_ops(&[set_exposure(root, node, 0.5)], Some(&start)).unwrap();
        assert!(old.is_none(), "it had none");
        assert_eq!(seq.root_grade(root).unwrap().unwrap().nodes.iter().find(|n| n.id == node).unwrap().params.exposure, Some(0.5));
    }

    /// A correction's mix and mask channel are refused on a sequence's own
    /// grades (#1321), and a track's grade isn't edited by path (#1408).
    #[test]
    fn mix_mask_channel_and_track_roots_are_refused() {
        let mut seq = Sequence::new_default();
        seq.timeline_grade_slot().set(Some(NodeGraph::default()));
        let root = Root::TimelineGrade { sequence: seq.id.0 };
        let node = corrector(seq.root_grade(root).unwrap().unwrap());
        let before = serde_json::to_value(seq.root_grade(root).unwrap()).unwrap();
        for op in [
            GraphOp::SetMix { path: NodePath::new(root, [node]), mix: Some(0.5) },
            GraphOp::SetMaskChannel { path: NodePath::new(root, [node]), channel: Some("red".into()) },
        ] {
            let why = seq.apply_root_grade_ops(&[op], None).unwrap_err();
            assert!(why.contains("#1321"), "{why}");
        }
        assert_eq!(serde_json::to_value(seq.root_grade(root).unwrap()).unwrap(), before, "nothing changed");
        let track = Root::Track { track: uuid::Uuid::nil(), part: klipp_graph_format::path::TrackPart::Grade };
        assert!(seq.apply_root_grade_ops(&[set_exposure(track, node, 0.5)], None).unwrap_err().contains("#1408"));
    }

    /// A grade as an editor leaves one: a Corrector with a mask, a disabled
    /// node with keys, a fan-out (serial and parallel edges) into a layer
    /// mixer whose second input comes by wireless link, labels, canvas
    /// positions, a frame round two nodes and a sticky note, and values on
    /// the nodes the edit doesn't touch. Its Corrector, the node the tests
    /// edit.
    fn rich() -> (NodeGraph, klipp_nodes::NodeId) {
        use klipp_nodes::{Edge, Node, NodeKind, NodePos};
        let node = |kind: NodeKind, label: &str| Node {
            id: klipp_nodes::NodeId::new_v4(),
            kind,
            label: label.into(),
            enabled: true,
            params: Default::default(),
            keyframes: Default::default(),
            output_link: None,
            input_links: Vec::new(),
        };
        let mut g = NodeGraph::default();
        let source = g.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Source).unwrap().id;
        let output = g.output_node_id;
        g.nodes.retain(|n| n.id == source || n.id == output);
        g.edges.clear();
        let mut base = node(NodeKind::Corrector, "Base");
        base.params.exposure = Some(0.25);
        base.params.contrast = Some(-0.1);
        base.params.masks = Some(klipp_core::color_grade::MaskStack { masks: vec![klipp_core::color_grade::Mask::default()] });
        let mut lift = node(NodeKind::Exposure, "Lift");
        lift.enabled = false;
        lift.params.exposure = Some(-0.5);
        let mut track = klipp_nodes::NodeKeyframeTrack { param_name: "exposure".into(), keyframes: Vec::new() };
        klipp_core::keyframe::upsert_keyframe(&mut track.keyframes, 0.0, -0.5);
        klipp_core::keyframe::upsert_keyframe(&mut track.keyframes, 1.5, 0.4);
        lift.keyframes.tracks.push(track);
        let mut look = node(NodeKind::Saturation, "Look");
        look.params.saturation = Some(1.2);
        look.output_link = Some("look".into());
        let mut mixer = node(NodeKind::LayerMixer, "Mix");
        mixer.input_links = vec![None, Some("look".into())];
        let (b, l, k, m) = (base.id, lift.id, look.id, mixer.id);
        let at = g.nodes.iter().position(|n| n.id == output).unwrap();
        for n in [base, lift, look, mixer].into_iter().rev() {
            g.nodes.insert(at, n);
        }
        let edge = |from_node, to_node, to_port| Edge { from_node, from_port: 0, to_node, to_port };
        g.edges = vec![edge(source, b, 0), edge(b, l, 0), edge(b, k, 0), edge(l, m, 0), edge(m, output, 0)];
        g.frames.push(klipp_nodes::NodeFrame { id: uuid::Uuid::new_v4(), label: "Primaries".into(), description: "base and lift".into(), color_rgba: 0x3366_99ff, contained_node_ids: [b, l].into_iter().collect() });
        g.sticky_notes.push(klipp_nodes::NodeStickyNote { id: uuid::Uuid::new_v4(), pos: NodePos { x: 40.0, y: -120.0 }, width: 240.0, text: "warm the highlights later".into(), color_rgba: 0xffee_88ff });
        for (i, id) in [source, b, l, k, m, output].into_iter().enumerate() {
            g.node_positions.insert(id, NodePos { x: 220.0 * i as f32, y: if id == k { 140.0 } else { 0.0 } });
        }
        (g, b)
    }

    /// A rich timeline grade and a rich post stage edited by path keep
    /// everything but the one value the edit sets: the stored grade, as its
    /// typed form serialises (not `canon`), is the one before with that
    /// field patched, and the same as the old route's in-place write of
    /// it. A write of the value it already has, and an empty batch, leave
    /// it as it was. Compared as JSON values, which don't depend on the
    /// order a hash map (the canvas positions) is written in.
    #[test]
    fn a_path_edit_changes_only_the_value_it_sets() {
        let mut seq = Sequence::new_default();
        let (grade, corrector) = rich();
        let mut group = GradeGroup::new("Night");
        group.stage_slot(GradeStage::Post).set(Some(grade.clone()));
        let gid = group.id.0;
        seq.grade_groups.push(group);
        seq.timeline_grade_slot().set(Some(grade.clone()));
        let json = |seq: &Sequence, root| serde_json::to_value(seq.root_grade(root).unwrap()).unwrap();
        for root in [Root::TimelineGrade { sequence: seq.id.0 }, Root::GroupStage { group: gid, stage: Stage::Post }] {
            let before = json(&seq, root);

            // A no-op first: the value it already has.
            seq.apply_root_grade_ops(&[set_exposure(root, corrector, 0.25)], None).unwrap();
            assert_eq!(json(&seq, root), before, "{root:?}: a write of the value it has changes nothing");
            assert!(seq.apply_root_grade_ops(&[], None).is_err(), "an empty batch is no edit");
            assert_eq!(json(&seq, root), before, "{root:?}: an empty batch changes nothing");

            seq.apply_root_grade_ops(&[set_exposure(root, corrector, 0.75)], None).unwrap();
            let mut patched = before.clone();
            let node = patched["nodes"].as_array_mut().unwrap().iter_mut().find(|n| n["id"] == serde_json::json!(corrector)).unwrap();
            node["params"]["exposure"] = serde_json::json!(0.75f32);
            assert_eq!(json(&seq, root), patched, "{root:?}: only that field changed");

            // The old route: the field written in place.
            let mut old = grade.clone();
            old.nodes.iter_mut().find(|n| n.id == corrector).unwrap().params.exposure = Some(0.75);
            // As values: `node_positions` is a hash map, whose order no two
            // maps share; the nodes and edges, lists, compare in order.
            assert_eq!(json(&seq, root), serde_json::to_value(&old).unwrap(), "{root:?}: the old route's grade");
        }
    }

}
