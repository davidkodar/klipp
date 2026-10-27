//! A sequence's own grades edited by path (M3 step 19): the timeline grade
//! (`Root::TimelineGrade`) and a grade group's pre-clip and post-clip
//! stages (`Root::GroupStage`). Each is kept as a v1 grade graph; a path
//! edit lifts it as paths address it, applies the ops there and lowers it
//! back, so the stored form is the one the old route writes.
//!
//! A correction's mix and gate mask channel, which a v1 graph has no
//! place for, are kept on its node as its run setting
//! (`correction_blend`), which the renderer reads as it does a clip
//! grade's, and saved in the grade's stored body (#1321,
//! [`klipp_graph_lift::sequence_grade_mix`]).
//!
//! A track's grade (`Root::Track` with `TrackPart::Grade`) is edited the
//! same way (#1408), kept as the track's v1 grade graph. It has no stored
//! form for a correction's mix or mask channel yet, so those are refused.

use crate::{GradeStage, Sequence};
use klipp_effects::mask_mix::MaskChannel;
use klipp_graph_format::lifted::Stage;
use klipp_graph_format::path::{Root, TrackPart};
use klipp_graph_format::Graph;
use klipp_graph_lift::ops::GraphOp;
use klipp_graph_lift::sequence_grade_mix::{self as grade_mix, Blend};
use klipp_nodes::params::EffectBlend;
use klipp_nodes::NodeGraph;

fn stage_of(stage: Stage) -> GradeStage {
    match stage {
        Stage::Pre => GradeStage::Pre,
        Stage::Post => GradeStage::Post,
    }
}

/// Why a path edit of `root` is refused before it is tried, if it is.
fn refused(root: Root) -> Option<String> {
    match root {
        Root::Track { part: TrackPart::Effects, .. } => Some("a track's effects aren't one of the sequence's grades".into()),
        _ => None,
    }
}

/// Every node of `grade`, each followed by those of its compound's
/// subgraph at any depth, as the grade's stored body orders them.
fn all_nodes(grade: &NodeGraph) -> Vec<&klipp_nodes::Node> {
    let mut out = Vec::new();
    for n in &grade.nodes {
        out.push(n);
        if let Some(sub) = &n.params.compound_subgraph {
            out.extend(all_nodes(sub));
        }
    }
    out
}

/// Each node of `grade`, inside its compounds too, whose run setting
/// holds a mix or a mask channel, as its stored body keeps them: whole
/// and red are left out.
pub fn blends_of(grade: &NodeGraph) -> Vec<Blend> {
    all_nodes(grade)
        .into_iter()
        .filter_map(|n| {
            let b = n.params.correction_blend?;
            let mix = (b.mix != 1.0).then_some(b.mix);
            let channel = (b.channel != MaskChannel::Red).then(|| b.channel.as_str().to_owned());
            (mix.is_some() || channel.is_some()).then_some(Blend { node: n.id, mix, channel })
        })
        .collect()
}

/// Set each node's run setting from `blends`, by node id, inside the
/// grade's compounds too: a node none names has none. A blend naming a
/// node `grade` doesn't hold, or a channel this build doesn't know, is
/// refused, and nothing is set.
pub fn set_blends(grade: &mut NodeGraph, blends: &[Blend]) -> Result<(), String> {
    let held: Vec<klipp_nodes::NodeId> = all_nodes(grade).iter().map(|n| n.id).collect();
    let mut settings = Vec::new();
    for b in blends {
        if !held.contains(&b.node) {
            return Err(format!("node {} holds a mix or mask channel but isn't in the grade", b.node));
        }
        let channel = match &b.channel {
            Some(c) => c.parse().map_err(|_| format!("node {}: unknown mask channel {c:?}", b.node))?,
            None => MaskChannel::Red,
        };
        let mix = b.mix.unwrap_or(1.0);
        settings.push((b.node, (mix != 1.0 || channel != MaskChannel::Red).then_some(EffectBlend { mix, channel })));
    }
    fn apply(grade: &mut NodeGraph, settings: &[(klipp_nodes::NodeId, Option<EffectBlend>)]) {
        for n in &mut grade.nodes {
            n.params.correction_blend = settings.iter().find(|(id, _)| *id == n.id).and_then(|(_, s)| *s);
            if let Some(sub) = n.params.compound_subgraph.as_mut() {
                apply(sub, settings);
            }
        }
    }
    apply(grade, &settings);
    Ok(())
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
            Root::Track { track, part: TrackPart::Grade } => Ok(self.tracks.iter().find(|t| t.id.0 == track).ok_or("no such track")?.effects.track_grade()),
            _ => Err("not one of this sequence's own grades".into()),
        }
    }

    /// The track `track`'s own effects and grade, lifted as its paths
    /// address them (M4 step 12e): the effect nodes, and the grade when it
    /// has one. A track root's two parts, which one lifted grade can't
    /// hold, beside [`Self::lift_root_grade`].
    pub fn lift_track_parts(&self, track: uuid::Uuid) -> Result<(Vec<klipp_graph_format::Node>, Option<Graph>), String> {
        let track = self.tracks.iter().find(|t| t.id.0 == track).ok_or("no such track")?;
        let raw = serde_json::to_value(track).map_err(|e| e.to_string())?;
        let structural = klipp_graph_lift::to_v2::track_to_v2(raw.as_object().ok_or("a track must be an object")?).map_err(|e| e.to_string())?;
        klipp_graph_lift::splice::track_parts(&structural)
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
            // A track's grade is a grade body addressed `[node]` as the
            // timeline grade's is, and lifts the same way: a track's own
            // lift leaves out a grade that changes nothing, which the
            // first edit of a fresh one starts from (#1408).
            Root::Track { track, part: TrackPart::Grade } => {
                self.tracks.iter().find(|t| t.id.0 == track).ok_or("no such track")?;
                let raw = serde_json::json!({ "id": track, "timeline_graph": grade });
                to_v2::timeline_to_v2(raw.as_object().expect("an object")).map_err(|e| e.to_string())?
            }
            _ => return Err("not one of this sequence's own grades".into()),
        };
        let structural = structural.ok_or("the grade lifts to nothing")?;
        let mut lifted = match root {
            Root::GroupStage { .. } => splice::stage_body(&structural)?,
            _ => lift::lift_grade_only(&structural, false)?,
        };
        // A correction's mix and mask channel, which the v1 graph's JSON
        // leaves out, on the nodes that hold them (#1321).
        grade_mix::set_blends(&mut lifted, &blends_of(grade))?;
        Ok(lifted)
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
            Root::Track { track, part: TrackPart::Grade } => {
                self.tracks.iter_mut().find(|t| t.id.0 == track).ok_or("no such track")?.effects.grade_slot().set(grade);
                Ok(())
            }
            _ => Err("not one of this sequence's own grades".into()),
        }
    }

    /// The mix and mask channel of each correction of `root`'s grade, as
    /// its stored body keeps them (#1321): none when it has no grade.
    pub fn root_blends(&self, root: Root) -> Vec<Blend> {
        self.root_grade(root).ok().flatten().map(blends_of).unwrap_or_default()
    }

    /// Set the mix and mask channel of `root`'s corrections from its
    /// stored body as read (#1321). Blends for a grade the sequence
    /// doesn't have, or a node it doesn't hold, are refused.
    pub fn set_root_blends(&mut self, root: Root, blends: &[Blend]) -> Result<(), String> {
        let mut grade = self.root_grade(root)?.cloned().ok_or("a mix or mask channel for a grade the sequence doesn't have")?;
        set_blends(&mut grade, blends)?;
        self.set_root_grade(root, Some(grade))
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
        if let Some(why) = refused(root) {
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
            Root::Track { track, .. } => {
                docs.track_grades.insert(track, lifted);
            }
            _ => unreachable!("refused above"),
        }
        klipp_graph_lift::ops::apply_batch_with_catalog(&mut docs, ops, &crate::RegistryCatalog)?;
        let edited = match root {
            Root::TimelineGrade { sequence } => docs.timeline.remove(&sequence),
            Root::GroupStage { group, stage } => docs.stages.remove(&(group, stage)),
            Root::Track { track, .. } => docs.track_grades.remove(&track),
            _ => None,
        }
        .ok_or("the edit lost its grade")?;
        let after = lowered_grade(&edited)?;
        self.set_root_grade(root, Some(after.clone()))?;
        Ok((before, after))
    }

    /// Apply `ops`, all on track `track`'s effect list (`Root::Track` with
    /// `TrackPart::Effects`), as one edit (#1214): the list lifted as its
    /// paths address it, the ops applied there with the registry's checks,
    /// and the result lowered to the track's own list. The track's effects
    /// before and after, for the edit's undo and redo, and each node the
    /// written list names by another id, as pairs of the batch's id and
    /// the list's (#1911): the list keeps no effect ids, so an effect is
    /// named by its place, and one added or moved takes its place's. A
    /// track's effects keep no keyframes, so a key op is refused; nothing
    /// is stored when any op is refused.
    pub fn apply_track_effect_ops(&mut self, ops: &[GraphOp]) -> Result<(crate::TrackEffects, crate::TrackEffects, Vec<(uuid::Uuid, uuid::Uuid)>), String> {
        let root = ops.first().ok_or("an empty batch has no edit")?.path().root;
        let Root::Track { track, part: TrackPart::Effects } = root else { return Err("not a track's effects".into()) };
        if ops.iter().any(|op| op.path().root != root) {
            return Err("a batch edits one picture".into());
        }
        if ops.iter().any(|op| matches!(op, GraphOp::AddKeyAt { .. } | GraphOp::RemoveKeyAt { .. } | GraphOp::SetAnim { .. })) {
            return Err("a track's effects keep no keyframes".into());
        }
        // Each effect's values in its map, as a clip's picture holds them
        // while it's edited, and back in its entry to be lowered (M3-2).
        let (mut nodes, _) = self.lift_track_parts(track)?;
        for n in &mut nodes {
            klipp_graph_lift::effect_params::into_memory(n)?;
        }
        let mut docs = klipp_graph_lift::paths::Documents::default();
        docs.track_effects.insert(track, nodes);
        klipp_graph_lift::ops::apply_batch_with_catalog(&mut docs, ops, &crate::RegistryCatalog)?;
        let mut nodes = docs.track_effects.remove(&track).ok_or("the edit lost the track's effects")?;
        let edited: Vec<uuid::Uuid> = nodes.iter().map(|n| n.id).collect();
        for n in &mut nodes {
            klipp_graph_lift::effect_params::into_stored(n)?;
        }
        // Lowered as a stored track's list is (`unlift`): a chain of them.
        let fields = match nodes.last().map(|n| n.id) {
            Some(last) => {
                let edges = nodes.windows(2).map(|w| klipp_graph_format::Edge { from: (w[0].id, "out".into()), to: (w[1].id, "in".into()) }).collect();
                klipp_graph_lift::to_v1::track(&Graph { nodes, edges, output: last, ..Graph::empty() })?
            }
            None => serde_json::json!({}),
        };
        let t = self.tracks.iter_mut().find(|t| t.id.0 == track).ok_or("no such track")?;
        let before = t.effects.clone();
        let mut v = serde_json::to_value(&t.effects).map_err(|e| e.to_string())?;
        let o = v.as_object_mut().ok_or("a track's effects must be an object")?;
        o.remove("video");
        o.remove("video_enabled");
        for (k, val) in fields.as_object().into_iter().flatten().filter(|(k, _)| matches!(k.as_str(), "video" | "video_enabled")) {
            o.insert(k.clone(), val.clone());
        }
        let new: crate::TrackEffects = serde_json::from_value(v).map_err(|e| e.to_string())?;
        let old = std::mem::replace(&mut t.effects, new);
        let written: Vec<uuid::Uuid> = match self.lift_track_parts(track) {
            Ok((nodes, _)) if nodes.len() == edited.len() => nodes.iter().map(|n| n.id).collect(),
            Ok(_) | Err(_) => {
                self.tracks.iter_mut().find(|t| t.id.0 == track).expect("found above").effects = old;
                return Err("the track's written list doesn't hold the edit's effects".into());
            }
        };
        let renamed = edited.into_iter().zip(written).filter(|(was, now)| was != now).collect();
        let t = self.tracks.iter().find(|t| t.id.0 == track).expect("found above");
        Ok((before, t.effects.clone(), renamed))
    }
}

/// A grade node of a lifted picture as the node a grade run takes (M4 step
/// 12e): through the lift's own way back, as the one node of a grade. For
/// a node beside a clip's chain (a mask producer), which the run takes on
/// its own.
pub fn grade_node_for_run(n: &klipp_graph_format::Node) -> Result<klipp_nodes::Node, String> {
    let body = Graph { nodes: vec![n.clone()], output: n.id, ..Graph::empty() };
    let graph = lowered_grade(&body)?;
    graph.nodes.into_iter().find(|m| m.id == n.id).ok_or_else(|| "the node didn't come back from its record".into())
}

/// A lifted grade body (a grade group's, a stage's, a grade compound's
/// #1746) as the grade the executor runs: lowered through its record, each
/// correction's mix and mask channel set as its run setting.
pub fn lowered_grade(body: &Graph) -> Result<NodeGraph, String> {
    let v1 = klipp_graph_lift::to_v1::grade_only(&klipp_graph_lift::delift::grade_only(body, klipp_graph_format::NodeId::nil())?)?;
    let mut graph: NodeGraph = serde_json::from_value(v1).map_err(|e| e.to_string())?;
    set_blends(&mut graph, &grade_mix::blends(body))?;
    Ok(graph)
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

    /// A path edit of a track's grade lands in the track's stored grade,
    /// as one of the sequence's own grades does (#1408): the value on the
    /// same node, every node id kept, the grade before handed back; a track
    /// without a grade takes its first edit on the graph drawn. A track's
    /// effects aren't a grade.
    #[test]
    fn a_path_edit_of_a_track_grade_lands_in_the_track() {
        let mut seq = Sequence::new_default();
        let tid = seq.tracks.iter().find(|t| t.kind == crate::TrackKind::Video).unwrap().id;
        let root = Root::Track { track: tid.0, part: TrackPart::Grade };
        let start = NodeGraph::default();
        let node = corrector(&start);
        assert!(seq.root_grade(root).unwrap().is_none(), "no grade yet");
        let (old, _) = seq.apply_root_grade_ops(&[set_exposure(root, node, 0.5)], Some(&start)).unwrap();
        assert!(old.is_none(), "it had none");
        let before = seq.root_grade(root).unwrap().cloned().unwrap();
        let (old, new) = seq.apply_root_grade_ops(&[set_exposure(root, node, 0.75)], None).unwrap();
        assert_eq!(old.map(|g| serde_json::to_value(g).unwrap()), Some(serde_json::to_value(&before).unwrap()), "the grade before");
        let track = seq.tracks.iter().find(|t| t.id == tid).unwrap();
        let stored = track.effects.track_grade().unwrap();
        assert_eq!(serde_json::to_value(stored).unwrap(), serde_json::to_value(&new).unwrap(), "stored on the track");
        assert_eq!(stored.nodes.iter().find(|n| n.id == node).unwrap().params.exposure, Some(0.75));
        let ids = |g: &NodeGraph| g.nodes.iter().map(|n| n.id).collect::<Vec<_>>();
        assert_eq!(ids(stored), ids(&before), "every node id kept");
        let effects = Root::Track { track: tid.0, part: TrackPart::Effects };
        assert!(seq.apply_root_grade_ops(&[set_exposure(effects, node, 0.5)], None).is_err(), "a track's effects aren't a grade");
        assert_eq!(Sequence::track_grade_clip_id(tid), Sequence::track_grade_clip_id(tid), "the stand-in is fixed");
        assert_eq!(seq.track_grade_of(Sequence::track_grade_clip_id(tid)), Some(tid));
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

    /// A correction's mix and mask channel on a sequence's own grade land
    /// on its node as its run setting (#1321), lifted with the grade for
    /// the next edit, and a later edit of another value keeps them; whole
    /// and red take the setting off. A track's grade has no stored form for
    /// them, so they're refused there (#1408).
    #[test]
    fn a_mix_and_mask_channel_land_and_track_roots_are_refused() {
        use klipp_effects::mask_mix::MaskChannel;
        let mut seq = Sequence::new_default();
        seq.timeline_grade_slot().set(Some(NodeGraph::default()));
        let root = Root::TimelineGrade { sequence: seq.id.0 };
        let node = corrector(seq.root_grade(root).unwrap().unwrap());
        let blend = |seq: &Sequence| seq.root_grade(root).unwrap().unwrap().nodes.iter().find(|n| n.id == node).unwrap().params.correction_blend;
        let path = NodePath::new(root, [node]);
        seq.apply_root_grade_ops(&[GraphOp::SetMix { path: path.clone(), mix: Some(0.5) }, GraphOp::SetMaskChannel { path: path.clone(), channel: Some("luminance".into()) }], None).unwrap();
        let set = Some(EffectBlend { mix: 0.5, channel: MaskChannel::Luminance });
        assert_eq!(blend(&seq), set);
        let lifted = seq.lift_root_grade(root, seq.root_grade(root).unwrap().unwrap()).unwrap();
        assert_eq!(grade_mix::blends(&lifted), vec![Blend { node, mix: Some(0.5), channel: Some("luminance".into()) }], "lifted with the grade");
        assert_eq!(seq.root_blends(root), grade_mix::blends(&lifted), "as its stored body keeps them");
        seq.apply_root_grade_ops(&[set_exposure(root, node, 0.25)], None).unwrap();
        assert_eq!(blend(&seq), set, "another value's edit keeps them");
        seq.apply_root_grade_ops(&[GraphOp::SetMix { path: path.clone(), mix: Some(1.0) }, GraphOp::SetMaskChannel { path, channel: Some("red".into()) }], None).unwrap();
        assert_eq!(blend(&seq), None, "whole and red is none");
        assert!(seq.root_blends(root).is_empty());
        let tid = seq.tracks[0].id;
        seq.tracks[0].effects.grade_slot().set(Some(NodeGraph::default()));
        let track = Root::Track { track: tid.0, part: TrackPart::Grade };
        let tnode = corrector(seq.root_grade(track).unwrap().unwrap());
        let before = seq.root_grade(track).unwrap().cloned();
        let why = seq.apply_root_grade_ops(&[GraphOp::SetMix { path: NodePath::new(track, [tnode]), mix: Some(0.5) }], None).unwrap_err();
        assert!(why.contains("mix"), "{why}");
        assert_eq!(seq.root_grade(track).unwrap().cloned(), before, "nothing stored");
    }

    /// Blends set on a grade by node id: a node none names has none, and
    /// one naming a node the grade doesn't hold or an unknown channel is
    /// refused.
    #[test]
    fn set_blends_is_by_node_and_refuses_what_it_cannot_place() {
        let mut g = NodeGraph::default();
        let node = corrector(&g);
        set_blends(&mut g, &[Blend { node, mix: Some(0.3), channel: None }]).unwrap();
        assert_eq!(blends_of(&g), vec![Blend { node, mix: Some(0.3), channel: None }]);
        set_blends(&mut g, &[]).unwrap();
        assert!(blends_of(&g).is_empty(), "none named, none kept");
        assert!(set_blends(&mut g, &[Blend { node: uuid::Uuid::new_v4(), mix: Some(0.3), channel: None }]).is_err());
        assert!(set_blends(&mut g, &[Blend { node, mix: None, channel: Some("green".into()) }]).is_err());
        // Inside a compound, at any depth.
        let inner = NodeGraph::default();
        let deep = corrector(&inner);
        let mut compound = g.nodes.iter().find(|n| n.id == node).unwrap().clone();
        compound.id = uuid::Uuid::new_v4();
        compound.kind = klipp_nodes::NodeKind::Compound;
        compound.params = Default::default();
        compound.params.compound_subgraph = Some(inner);
        g.nodes.push(compound);
        let both = [Blend { node, mix: Some(0.3), channel: None }, Blend { node: deep, mix: None, channel: Some("alpha".into()) }];
        set_blends(&mut g, &both).unwrap();
        assert_eq!(blends_of(&g), both.to_vec());
        set_blends(&mut g, &[]).unwrap();
        assert!(blends_of(&g).is_empty(), "the compound's cleared too");
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
        g.frames.push(klipp_nodes::NodeFrame { id: uuid::Uuid::new_v4(), label: "Primaries".into(), description: "base and lift".into(), color_rgba: 0x3366_99ff, contained_node_ids: [b, l].into_iter().collect(), recipe: None });
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
