//! The grade accessor (#1103): the one way to reach a grade graph, on
//! every owner of one, as the effect-list API (#1064) is for effect lists.
//!
//! Additive for now: the fields stay public and no caller has moved. The
//! switch's stage B swaps the storage to the revision-2 model under these
//! methods, and `edit_grade`'s body becomes "apply as graph ops by path,
//! check, snapshot", so the callers that move onto it (#1104, #1105) don't
//! move again.
//!
//! The owners:
//! - always graded, by [`GradeAccess`]: a clip's own grade, a grade
//!   version, a Look;
//! - graded or not, by a [`GradeSlot`]: the timeline grade
//!   ([`Sequence::timeline_grade_slot`]), a grade group's stages
//!   ([`GradeGroup::stage_slot`]), a track's grade
//!   ([`TrackEffects::grade_slot`]).
//!
//! The named edits are today's, step for step: a parameter set as the
//! engine's drain sets it, a node removed and a wire connected as the
//! Colour page's commands do.

use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind};

use crate::{Clip, GradeGroup, GradeStage, GradeVersion, Look, Sequence, TrackEffects};

/// How deep a parameter set looks into Compound sub-graphs, as the
/// engine's drain does (`COMPOUND_MAX_DEPTH_ENGINE`).
const COMPOUND_DEPTH: u32 = 8;

/// Set `param` of `node` to `value`, in the graph or a Compound's
/// sub-graph inside it: whether the node was found.
fn set_param(g: &mut NodeGraph, node: NodeId, param: &str, value: f32, depth: u32) -> bool {
    if depth >= COMPOUND_DEPTH {
        return false;
    }
    if let Some(n) = g.nodes.iter_mut().find(|n| n.id == node) {
        n.params.apply_override(param, value);
        return true;
    }
    g.nodes
        .iter_mut()
        .filter(|n| n.kind == NodeKind::Compound)
        .filter_map(|n| n.params.compound_subgraph.as_mut())
        .any(|sub| set_param(sub, node, param, value, depth + 1))
}

/// Add `node`: unwired, as the Colour page adds one, or with `after`, in
/// the chain after that node, taking over what its main output fed.
/// Whether it landed (`after` names a node of the graph).
fn insert_node(g: &mut NodeGraph, after: Option<NodeId>, node: Node) -> bool {
    let id = node.id;
    let Some(after) = after else {
        g.nodes.push(node);
        return true;
    };
    let Some(at) = g.nodes.iter().position(|n| n.id == after) else { return false };
    for e in g.edges.iter_mut().filter(|e| e.from_node == after && e.from_port == 0) {
        e.from_node = id;
    }
    if g.output_node_id == after {
        g.output_node_id = id;
    }
    g.edges.push(Edge { from_node: after, from_port: 0, to_node: id, to_port: 0 });
    g.nodes.insert(at + 1, node);
    true
}

/// Remove `node` and its wires, its place on the canvas and its frames'
/// hold on it, as the Colour page's `RemoveNode` does: a frame left
/// holding nothing goes too.
fn remove_node(g: &mut NodeGraph, node: NodeId) {
    g.nodes.retain(|n| n.id != node);
    g.edges.retain(|e| e.from_node != node && e.to_node != node);
    g.node_positions.remove(&node);
    for frame in &mut g.frames {
        frame.contained_node_ids.remove(&node);
    }
    g.frames.retain(|f| !f.contained_node_ids.is_empty());
}

/// Wire `from`'s output to `to`'s input, as the Colour page's
/// `ConnectNodes` does: the port's old wire goes, a Layer Mixer grows to
/// the port, and a wire that would loop is not made. Whether it was.
fn connect(g: &mut NodeGraph, from: NodeId, from_port: u8, to: NodeId, to_port: u8) -> bool {
    let before = g.clone();
    g.edges.retain(|e| !(e.to_node == to && e.to_port == to_port));
    g.edges.push(Edge { from_node: from, from_port, to_node: to, to_port });
    if let Some(target) = g.nodes.iter_mut().find(|n| n.id == to && n.kind == NodeKind::LayerMixer) {
        let mixer = target.params.layer_mixer.get_or_insert_with(Default::default);
        if mixer.inputs.len() < usize::from(to_port) + 1 {
            mixer.inputs.resize_with(usize::from(to_port) + 1, klipp_nodes::LayerMixerInput::default);
        }
    }
    if g.topo_sort().is_err() {
        *g = before;
        return false;
    }
    true
}

/// Take the wire into `to`'s input `to_port` away.
fn disconnect(g: &mut NodeGraph, to: NodeId, to_port: u8) {
    g.edges.retain(|e| !(e.to_node == to && e.to_port == to_port));
}

/// An owner that always has a grade: a clip, a grade version, a Look.
pub trait GradeAccess {
    /// The grade.
    fn grade(&self) -> &NodeGraph;

    /// Change the grade in `f`, and give back what `f` returns.
    fn edit_grade<R>(&mut self, f: impl FnOnce(&mut NodeGraph) -> R) -> R;

    /// Put `grade` in place of the grade.
    fn set_grade(&mut self, grade: NodeGraph) {
        self.edit_grade(|g| *g = grade);
    }

    /// Set `param` of `node` to `value`: whether the node is in the grade.
    fn set_grade_param(&mut self, node: NodeId, param: &str, value: f32) -> bool {
        self.edit_grade(|g| set_param(g, node, param, value, 0))
    }

    /// Add `node`, unwired, or in the chain after `after`: whether it landed.
    fn insert_grade_node(&mut self, after: Option<NodeId>, node: Node) -> bool {
        self.edit_grade(|g| insert_node(g, after, node))
    }

    /// Remove `node` and its wires.
    fn remove_grade_node(&mut self, node: NodeId) {
        self.edit_grade(|g| remove_node(g, node));
    }

    /// Wire `from` into `to`, unless the wire would loop: whether it was made.
    fn connect_grade(&mut self, from: NodeId, from_port: u8, to: NodeId, to_port: u8) -> bool {
        self.edit_grade(|g| connect(g, from, from_port, to, to_port))
    }

    /// Take the wire into `to`'s input `to_port` away.
    fn disconnect_grade(&mut self, to: NodeId, to_port: u8) {
        self.edit_grade(|g| disconnect(g, to, to_port));
    }
}

impl GradeAccess for Clip {
    fn grade(&self) -> &NodeGraph {
        &self.node_graph
    }
    fn edit_grade<R>(&mut self, f: impl FnOnce(&mut NodeGraph) -> R) -> R {
        f(&mut self.node_graph)
    }
}

impl GradeVersion {
    /// A grade version named `name`, holding `grade`.
    pub fn new(name: impl Into<String>, grade: NodeGraph) -> Self {
        Self { name: name.into(), node_graph: grade }
    }
}

impl Look {
    /// A Look with its id and name, holding `grade`.
    pub fn new(id: uuid::Uuid, name: impl Into<String>, grade: NodeGraph) -> Self {
        Self { id, name: name.into(), node_graph: grade }
    }
}

impl GradeAccess for GradeVersion {
    fn grade(&self) -> &NodeGraph {
        &self.node_graph
    }
    fn edit_grade<R>(&mut self, f: impl FnOnce(&mut NodeGraph) -> R) -> R {
        f(&mut self.node_graph)
    }
}

impl GradeAccess for Look {
    fn grade(&self) -> &NodeGraph {
        &self.node_graph
    }
    fn edit_grade<R>(&mut self, f: impl FnOnce(&mut NodeGraph) -> R) -> R {
        f(&mut self.node_graph)
    }
}

/// A grade that may be absent: the timeline grade, a grade group's stage,
/// a track's grade. The named edits change nothing, and say so, where
/// there is no grade.
pub struct GradeSlot<'a>(&'a mut Option<NodeGraph>);

impl GradeSlot<'_> {
    /// The grade, if there is one.
    pub fn get(&self) -> Option<&NodeGraph> {
        self.0.as_ref()
    }

    /// Put `grade` in place, or take the grade away with `None`.
    pub fn set(&mut self, grade: Option<NodeGraph>) {
        *self.0 = grade;
    }

    /// Change the grade in `f`, if there is one, and give back what `f`
    /// returns.
    pub fn edit<R>(&mut self, f: impl FnOnce(&mut NodeGraph) -> R) -> Option<R> {
        self.0.as_mut().map(f)
    }

    /// Change the grade in `f`, starting an empty one if there is none,
    /// as the engine's drain starts the timeline grade on a first drag.
    pub fn edit_or_insert<R>(&mut self, f: impl FnOnce(&mut NodeGraph) -> R) -> R {
        f(self.0.get_or_insert_with(Default::default))
    }

    /// Set `param` of `node` to `value`: whether there is a grade and the
    /// node is in it.
    pub fn set_param(&mut self, node: NodeId, param: &str, value: f32) -> bool {
        self.edit(|g| set_param(g, node, param, value, 0)).unwrap_or(false)
    }

    /// Add `node`, unwired, or in the chain after `after`: whether it landed.
    pub fn insert_node(&mut self, after: Option<NodeId>, node: Node) -> bool {
        self.edit(|g| insert_node(g, after, node)).unwrap_or(false)
    }

    /// Remove `node` and its wires.
    pub fn remove_node(&mut self, node: NodeId) {
        self.edit(|g| remove_node(g, node));
    }

    /// Wire `from` into `to`, unless the wire would loop: whether it was made.
    pub fn connect(&mut self, from: NodeId, from_port: u8, to: NodeId, to_port: u8) -> bool {
        self.edit(|g| connect(g, from, from_port, to, to_port)).unwrap_or(false)
    }

    /// Take the wire into `to`'s input `to_port` away.
    pub fn disconnect(&mut self, to: NodeId, to_port: u8) {
        self.edit(|g| disconnect(g, to, to_port));
    }
}

impl Sequence {
    /// Every grade the sequence keeps: the timeline's, each grade group's
    /// two stages, each track's grade, and each clip's grade and its saved
    /// versions.
    pub fn grades(&self) -> Vec<&NodeGraph> {
        let mut out: Vec<&NodeGraph> = self.timeline_graph.iter().collect();
        for g in &self.grade_groups {
            out.extend(g.pre.iter().chain(g.post.iter()));
        }
        for track in &self.tracks {
            out.extend(track.effects.grade.iter());
            for clip in &track.clips {
                out.push(&clip.node_graph);
                out.extend(clip.grade_versions.iter().map(|v| &v.node_graph));
            }
        }
        out
    }

    /// Change every grade of [`Sequence::grades`] in `f`, in that order.
    pub fn edit_each_grade(&mut self, mut f: impl FnMut(&mut NodeGraph)) {
        self.timeline_graph.iter_mut().for_each(&mut f);
        for g in &mut self.grade_groups {
            g.pre.iter_mut().chain(g.post.iter_mut()).for_each(&mut f);
        }
        for track in &mut self.tracks {
            track.effects.grade.iter_mut().for_each(&mut f);
            for clip in &mut track.clips {
                f(&mut clip.node_graph);
                clip.grade_versions.iter_mut().for_each(|v| f(&mut v.node_graph));
            }
        }
    }

    /// The timeline grade, if the sequence has one.
    pub fn timeline_grade(&self) -> Option<&NodeGraph> {
        self.timeline_graph.as_ref()
    }

    /// The timeline grade, to change.
    pub fn timeline_grade_slot(&mut self) -> GradeSlot<'_> {
        GradeSlot(&mut self.timeline_graph)
    }
}

impl GradeGroup {
    /// A stage's grade, to change.
    pub fn stage_slot(&mut self, stage: GradeStage) -> GradeSlot<'_> {
        GradeSlot(self.stage_mut(stage))
    }
}

impl TrackEffects {
    /// The track's grade, if it has one.
    pub fn track_grade(&self) -> Option<&NodeGraph> {
        self.grade.as_ref()
    }

    /// The track's grade, to change.
    pub fn grade_slot(&mut self) -> GradeSlot<'_> {
        GradeSlot(&mut self.grade)
    }
}

#[cfg(test)]
#[path = "grade_access_tests.rs"]
mod tests;
