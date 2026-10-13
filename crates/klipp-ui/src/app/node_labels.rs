//! Colour labels on graph nodes (#1753), between the canvases and the
//! sequence. The canvases name a node as the clip they draw names it; a
//! grade the sequence owns (its timeline grade, a grade group's stage, a
//! track's grade) is drawn through a stand-in clip, and its labels are
//! kept under that grade, where they stay whichever way it is opened.

use klipp_graph_format::lifted::Stage;
use klipp_graph_format::path::{NodePath, Root, TrackPart};
use klipp_timeline::{grade_group_id, GradeStage, Sequence};
use std::collections::HashMap;

/// The sequence's own grades with the clip each is drawn through.
fn stand_ins(seq: &Sequence) -> Vec<(klipp_core::ClipId, Root)> {
    let mut out = vec![(seq.timeline_grade_clip_id(), Root::TimelineGrade { sequence: seq.id.0 })];
    for g in &seq.grade_groups {
        out.push((g.stage_clip_id(GradeStage::Pre), Root::GroupStage { group: g.id.0, stage: Stage::Pre }));
        out.push((g.stage_clip_id(GradeStage::Post), Root::GroupStage { group: g.id.0, stage: Stage::Post }));
    }
    for t in &seq.tracks {
        out.push((Sequence::track_grade_clip_id(t.id), Root::Track { track: t.id.0, part: TrackPart::Grade }));
    }
    out
}

/// Where a label on the node a canvas names `canvas` is kept: a stand-in's
/// node under the grade it stands in for, any other as it is.
pub(super) fn stored_path(seq: &Sequence, canvas: &NodePath) -> NodePath {
    if let Root::Clip { clip } = canvas.root {
        if let Some((id, root)) = stand_ins(seq).into_iter().find(|(id, _)| id.0 == clip) {
            if canvas.nodes.first() == Some(&grade_group_id(id)) {
                return NodePath::new(root, canvas.nodes[1..].iter().copied());
            }
        }
    }
    canvas.clone()
}

/// Each label of `seq` by the path a canvas names its node by.
pub(crate) fn canvas_labels(seq: &Sequence) -> HashMap<NodePath, u8> {
    let stand_ins = stand_ins(seq);
    seq.node_labels
        .iter()
        .map(|l| {
            let path = match stand_ins.iter().find(|(_, root)| *root == l.node.root) {
                Some((id, _)) => NodePath::new(Root::Clip { clip: id.0 }, std::iter::once(grade_group_id(*id)).chain(l.node.nodes.iter().copied())),
                None => l.node.clone(),
            };
            (path, l.colour)
        })
        .collect()
}

impl crate::state::AppState {
    /// The node at `node` given `colour` (0 takes its label off), one
    /// undo step: whether anything changed.
    pub(crate) fn set_node_colour_label(&mut self, node: &NodePath, colour: u8) -> bool {
        let seq = &mut self.sequences[self.active_sequence_idx];
        let stored = stored_path(seq, node);
        let new = seq.node_labels_with(&stored, colour);
        if new == seq.node_labels {
            return false;
        }
        let old = std::mem::replace(&mut seq.node_labels, new.clone());
        self.push_edits_as_one(&[crate::history::EditCommand::SetNodeLabels { old, new }], "Node Colour Label");
        self.dirty = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_timeline::GradeGroup;

    /// A label set through a stand-in is kept under the grade it stands
    /// in for and drawn back on the stand-in's node; a clip's own node
    /// keeps its path both ways.
    #[test]
    fn stand_in_paths_go_both_ways() {
        let mut seq = Sequence::new_default();
        let group = GradeGroup::new("Interview");
        let (gid, post) = (group.id, group.stage_clip_id(GradeStage::Post));
        seq.grade_groups.push(group);
        let node = uuid::Uuid::new_v4();
        let timeline = seq.timeline_grade_clip_id();
        let track = seq.tracks[0].id;
        let track_clip = Sequence::track_grade_clip_id(track);
        let own = klipp_core::ClipId::new();
        let cases = [
            (timeline, Root::TimelineGrade { sequence: seq.id.0 }),
            (post, Root::GroupStage { group: gid.0, stage: Stage::Post }),
            (track_clip, Root::Track { track: track.0, part: TrackPart::Grade }),
        ];
        for (clip, root) in cases {
            let canvas = NodePath::new(Root::Clip { clip: clip.0 }, [grade_group_id(clip), node]);
            let stored = stored_path(&seq, &canvas);
            assert_eq!(stored, NodePath::new(root, [node]));
            seq.node_labels = seq.node_labels_with(&stored, 2);
            assert_eq!(canvas_labels(&seq).get(&canvas), Some(&2));
            seq.node_labels.clear();
        }
        let canvas = NodePath::new(Root::Clip { clip: own.0 }, [grade_group_id(own), node]);
        assert_eq!(stored_path(&seq, &canvas), canvas);
    }
}
