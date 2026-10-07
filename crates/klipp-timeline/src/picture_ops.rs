//! A clip's picture edited by graph ops (node-graph M2 step 15a).
//!
//! Every batch is applied first to a copy of the clip's lifted picture
//! (`klipp_graph_lift::ops`, checked against the registries): a refusal
//! there is the answer, and the clip is untouched. Then the batch is
//! written to the clip's own fields, which are the truth while a clip's
//! picture is stored as revision-1 fields, through the clip's own
//! mutators, so the lifted picture is dropped and lifted again from what
//! was written:
//! - an op inside the grade writes the grade back whole, from the edited
//!   copy (`delift`, then `to_v1`'s node graph);
//! - an op on a built-in effect entry goes to the effect-list mutators,
//!   which carry its flag, keyframes and modifier stacks with it.
//!
//! The rest is refused with a reason rather than written half: a node the
//! lift places itself (opacity, transform, speed ramp, temporal denoise),
//! a plug-in entry, an effect's name or place in the node editor (no
//! revision-1 field holds them), and any move of an entry other nodes
//! read by id.
//!
//! The effect-list half of this goes when a clip's picture is stored as
//! its revision-2 picture (picture-truth clips the norm, `m3-decisions`
//! proposal 6): the edited copy is then the clip's picture itself.

use crate::{Clip, ClipPictureFields, GradeAccess, RegistryCatalog};
use klipp_graph_format::lifted::{types as lt, ClipPicture};
use klipp_graph_format::path::Root;
use klipp_graph_format::{types, Node, NodeId, ParamValue};
use klipp_graph_lift::ops::{apply_in_batch, check_batch_end, GraphOp};
use klipp_graph_lift::paths::Documents;
use serde_json::Value;

/// What a batch of ops did to a clip's picture: its fields before and
/// after, effect ids included, so undo and redo put it back whole.
#[derive(Clone, Debug)]
pub struct PictureEdit {
    pub before: ClipPictureFields,
    pub after: ClipPictureFields,
}

/// The node an op names.
fn path_of(op: &GraphOp) -> &klipp_graph_format::path::NodePath {
    match op {
        GraphOp::AddNode { after, .. } => after,
        GraphOp::AddNodeBeside { at, .. } => at,
        GraphOp::RemoveNode { path }
        | GraphOp::SetParam { path, .. }
        | GraphOp::SetEnabled { path, .. }
        | GraphOp::SetMix { path, .. }
        | GraphOp::SetMaskChannel { path, .. }
        | GraphOp::MoveAfter { path, .. }
        | GraphOp::SetNodeMeta { path, .. }
        | GraphOp::AddKeyAt { path, .. }
        | GraphOp::RemoveKeyAt { path, .. }
        | GraphOp::SetAnim { path, .. }
        | GraphOp::SetPayload { path, .. } => path,
        GraphOp::Connect { to, .. } | GraphOp::Disconnect { to, .. } => to,
        GraphOp::PublishOutput { group, .. } | GraphOp::Unpublish { group, .. } => group,
    }
}

/// An op that adds, removes or moves a node.
fn is_structural(op: &GraphOp) -> bool {
    matches!(op, GraphOp::AddNode { .. } | GraphOp::AddNodeBeside { .. } | GraphOp::RemoveNode { .. } | GraphOp::MoveAfter { .. })
}

/// Where an op lands on the clip's fields.
enum Target {
    /// Inside the grade group: the grade is written back whole.
    Grade,
    /// A built-in effect entry, by its node.
    Effect,
}

fn node(p: &ClipPicture, id: NodeId) -> Option<&Node> {
    p.graph.nodes.iter().find(|n| n.id == id)
}

/// Where `op` lands, or why it can't be written to the fields.
fn target(p: &ClipPicture, op: &GraphOp) -> Result<Target, String> {
    let path = path_of(op);
    let first = path.nodes.first().copied().ok_or("an empty path names no node")?;
    let first = node(p, first).ok_or_else(|| format!("{first} isn't in the clip's picture"))?;
    // A node added in the clip's own chain: what it is decides, not what
    // it follows (the grade group, the display transform).
    let in_chain = path.nodes.len() == 1;
    if first.type_id.as_str() == lt::GROUP && !(in_chain && matches!(op, GraphOp::AddNode { .. })) {
        if path.nodes.len() < 2 {
            return Err("the grade group itself is set by the nodes inside it".into());
        }
        return Ok(Target::Grade);
    }
    if path.nodes.len() != 1 {
        return Err(format!("{} has no inside to reach into", first.id));
    }
    let entry = |n: &Node| -> Result<(), String> {
        match n.type_id.as_str() {
            types::LEGACY_EFFECT => Ok(()),
            types::LEGACY_OFX => Err("a plug-in effect is edited in its own panel until plug-ins become nodes".into()),
            lt::SOURCE | lt::OUTPUT | lt::SCENE_TO_DISPLAY => Err("a picture's ends aren't edited".into()),
            other => Err(format!("a {other} is placed by the clip, not edited as a node, while the clip's picture is stored as fields")),
        }
    };
    match op {
        // The new node is the effect; it goes after the named one.
        GraphOp::AddNode { node: new, .. } => {
            entry(new)?;
            Ok(Target::Effect)
        }
        GraphOp::SetParam { .. } | GraphOp::SetPayload { .. } | GraphOp::SetEnabled { .. } | GraphOp::RemoveNode { .. } | GraphOp::MoveAfter { .. } | GraphOp::AddKeyAt { .. } | GraphOp::RemoveKeyAt { .. } | GraphOp::SetAnim { .. } => {
            entry(first)?;
            Ok(Target::Effect)
        }
        GraphOp::SetNodeMeta { .. } => Err("an effect's name and place aren't kept until effects become nodes".into()),
        GraphOp::AddNodeBeside { .. } => Err("a node beside the clip's own chain waits for clips whose picture is stored as their revision-2 picture".into()),
        GraphOp::SetMix { .. } => Err("an effect's mix isn't kept until effects become nodes".into()),
        _ => Err("an effect in the clip's chain has nothing to wire".into()),
    }
}

/// Whether anything reads a built-in effect node by its id other than as
/// the next picture in the chain: a matte into a mask, a wireless link, a
/// published port. Moving such an entry would renumber it under the
/// reference (its id comes from its place in the list).
fn read_by_id(p: &ClipPicture) -> Option<NodeId> {
    let effect = |id: NodeId| node(p, id).is_some_and(|n| n.type_id.as_str() == types::LEGACY_EFFECT);
    if let Some(e) = p.graph.edges.iter().find(|e| effect(e.from.0) && e.to.1.as_str() != "in") {
        return Some(e.from.0);
    }
    if let Some(n) = p.graph.nodes.iter().find(|n| effect(n.id) && n.wireless.output_link.is_some()) {
        return Some(n.id);
    }
    p.graph.nodes.iter().filter_map(|n| n.group.as_ref()).flat_map(|g| &g.interface).map(|port| port.binding.node).find(|id| effect(*id))
}

/// The built-in effect entries in list order (by their stored slot), each
/// by its node; a slot whose entry the lift didn't keep as a node is
/// `None`.
fn list_order(p: &ClipPicture, len: usize) -> Vec<Option<NodeId>> {
    let mut order = vec![None; len];
    for n in p.graph.nodes.iter().filter(|n| n.type_id.as_str() != types::LEGACY_OFX) {
        if let Some(slot) = n.legacy.as_ref().and_then(|l| l.slot) {
            if let Some(at) = order.get_mut(slot as usize) {
                *at = Some(n.id);
            }
        }
    }
    order
}

/// The clip's chain in picture order, walked along its edges from the
/// source (each node's output into the next one's picture input), never
/// read from the order the node list happens to hold.
fn chain(p: &ClipPicture) -> Vec<&Node> {
    let mut out = Vec::with_capacity(p.graph.nodes.len());
    let mut at = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE);
    while let Some(n) = at {
        if out.iter().any(|seen: &&Node| seen.id == n.id) {
            break;
        }
        out.push(n);
        at = p.graph.edges.iter().find(|e| e.from.0 == n.id && e.from.1.as_str() == "out" && e.to.1.as_str() == "in").and_then(|e| node(p, e.to.0));
    }
    out
}

/// Where a node now in the chain sits in the list: just before the next
/// entry after it in the chain, or at the end. `None` when the chain puts
/// it where the list can't: before the grade, the display transform or a
/// plug-in, all of which a clip's built-in effects follow.
fn list_place(p: &ClipPicture, order: &[Option<NodeId>], id: NodeId) -> Option<usize> {
    let chain = chain(p);
    let at = chain.iter().position(|n| n.id == id)?;
    if chain[at + 1..].iter().any(|n| matches!(n.type_id.as_str(), lt::GROUP | lt::SCENE_TO_DISPLAY | types::LEGACY_OFX)) {
        return None;
    }
    let next = chain[at + 1..].iter().filter(|n| n.id != id).find_map(|n| order.iter().position(|o| *o == Some(n.id)));
    Some(next.unwrap_or(order.len()))
}

/// A parameter value as an effect stores it.
fn stored(value: &ParamValue) -> Result<Value, String> {
    Ok(match value {
        ParamValue::Float(f) => serde_json::json!(f),
        ParamValue::Int(i) => serde_json::json!(i),
        ParamValue::Bool(b) => serde_json::json!(b),
        ParamValue::Enum(e) => serde_json::json!(e),
        ParamValue::Text(t) => serde_json::json!(t),
        ParamValue::Point2(p) => serde_json::json!(p),
        ParamValue::Color { .. } | ParamValue::Matrix3(_) => return Err("that value is set through the effect's own editor".into()),
    })
}

fn tag_of(n: &Node) -> &str {
    n.legacy.as_ref().and_then(|l| l.payload.get("type")).and_then(Value::as_str).unwrap_or("")
}

fn key_of(v: &Value) -> Result<klipp_core::Keyframe, String> {
    serde_json::from_value(v.clone()).map_err(|e| format!("a key: {e}"))
}

/// Whether `effect` is an entry graph ops write: a plain effect node, not
/// one of the four the clip places itself. The writers route by it, with
/// the conversion's own rule (`to_v2::effect_node_type`), so they and the
/// refusal here can't disagree.
pub fn takes_picture_ops(effect: &klipp_effects::BuiltinEffect) -> bool {
    let tag = serde_json::to_value(effect).ok().and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_owned)).unwrap_or_default();
    klipp_graph_lift::to_v2::effect_node_type(&tag) == types::LEGACY_EFFECT
}

/// An edit of a clip's built-in effect list as today's commands name it:
/// by place in the list.
#[derive(Clone, Copy, Debug)]
pub enum EffectEdit<'a> {
    /// Put an effect at the end of the list.
    Add(&'a klipp_effects::BuiltinEffect),
    Remove(usize),
    SetEnabled(usize, bool),
    /// The effect at a place, replaced whole (its id, flag and keys stay).
    Update(usize, &'a klipp_effects::BuiltinEffect),
    /// Move the effect at `from` to place `to` (`to` counted after it is
    /// taken out), its flag, keys and stacks with it.
    Move { from: usize, to: usize },
}

impl Clip {
    /// The graph ops for `edit`, when every entry it touches is a plain
    /// effect node ([`takes_picture_ops`]); `None` when one is a placed
    /// kind or the list can't be read that way, and the edit stays on the
    /// effect-list mutators (picture-truth clips the norm ends that).
    pub fn effect_edit_ops(&self, edit: EffectEdit) -> Option<Vec<GraphOp>> {
        let lifted = self.picture_v2().ok()?;
        let p = &lifted.picture;
        let order = list_order(p, self.builtin_effects().len());
        let plain = |i: usize| -> Option<NodeId> {
            let id = (*order.get(i)?)?;
            node(p, id).filter(|n| n.type_id.as_str() == types::LEGACY_EFFECT).map(|n| n.id)
        };
        let at = |id: NodeId| klipp_graph_format::path::NodePath::new(Root::Clip { clip: self.id.0 }, [id]);
        // The chain node a new or moved effect goes after to be first in
        // the list: the display transform, or the last plug-in after it.
        let before_first = || {
            let chain = chain(p);
            let from = chain.iter().position(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY)?;
            chain[from..].iter().take_while(|n| matches!(n.type_id.as_str(), lt::SCENE_TO_DISPLAY | types::LEGACY_OFX)).last().map(|n| n.id)
        };
        Some(match edit {
            EffectEdit::Add(effect) => {
                if !takes_picture_ops(effect) {
                    return None;
                }
                // After the list's last entry, which must be a plain effect
                // (or none), so the new one lands at the end as a push does.
                let after = match self.builtin_effects().len() {
                    0 => before_first()?,
                    n => plain(n - 1)?,
                };
                let payload = serde_json::to_value(effect).ok()?;
                let new = Node {
                    id: uuid::Uuid::new_v4(),
                    type_id: types::LEGACY_EFFECT.into(),
                    type_version: 1,
                    label: String::new(),
                    enabled: true,
                    params: Default::default(),
                    anim: Default::default(),
                    mix: None,
                    wireless: Default::default(),
                    group: None,
                    legacy: Some(klipp_graph_format::Legacy { slot: None, payload, keyframes: None, modifiers: Default::default() }),
                    legacy_flags: None,
                    origin: None,
                };
                vec![GraphOp::AddNode { after: at(after), node: Box::new(new) }]
            }
            EffectEdit::Remove(i) => vec![GraphOp::RemoveNode { path: at(plain(i)?) }],
            EffectEdit::SetEnabled(i, enabled) => vec![GraphOp::SetEnabled { path: at(plain(i)?), enabled }],
            EffectEdit::Update(i, effect) => {
                if !takes_picture_ops(effect) {
                    return None;
                }
                vec![GraphOp::SetPayload { path: at(plain(i)?), payload: serde_json::to_value(effect).ok()? }]
            }
            EffectEdit::Move { from, to } => {
                let moved = plain(from)?;
                let mut rest = order.clone();
                rest.remove(from);
                let after = match to {
                    0 => before_first()?,
                    _ => {
                        let id = (*rest.get(to - 1)?)?;
                        node(p, id).filter(|n| n.type_id.as_str() == types::LEGACY_EFFECT)?.id
                    }
                };
                vec![GraphOp::MoveAfter { path: at(moved), after }]
            }
        })
    }

    /// `edits` in turn, each built against the clip as the ones before
    /// left it, as one edit of the clip's picture: what a document op that
    /// takes an effect off and puts another on records as one step. `None`
    /// when one of them doesn't take graph ops (a placed kind), and the
    /// clip is as it was; the caller's mutator path runs instead.
    pub fn apply_effect_edits(&mut self, edits: &[EffectEdit]) -> Option<Result<PictureEdit, String>> {
        let before = self.picture.clone();
        for edit in edits {
            let Some(ops) = self.effect_edit_ops(*edit) else {
                self.picture = before;
                return None;
            };
            if let Err(why) = self.apply_picture_ops(&ops) {
                self.picture = before;
                return Some(Err(why));
            }
        }
        Some(Ok(PictureEdit { before, after: self.picture.clone() }))
    }

    /// Apply `ops` to the clip's picture as one edit, checked first on a
    /// copy of its lifted picture. A refusal leaves the clip as it was.
    pub fn apply_picture_ops(&mut self, ops: &[GraphOp]) -> Result<PictureEdit, String> {
        self.apply_picture_ops_indexed(ops).map_err(|(_, why)| why)
    }

    /// Apply one atomic batch, retaining the index of an operation-level refusal.
    /// Whole-picture failures have no index unless an unwired node was added here.
    pub fn apply_picture_ops_indexed(&mut self, ops: &[GraphOp]) -> Result<PictureEdit, (Option<usize>, String)> {
        if ops.is_empty() {
            return Err((None, "an empty batch has no edit".into()));
        }
        let root = Root::Clip { clip: self.id.0 };
        if let Some((index, op)) = ops.iter().enumerate().find(|(_, op)| path_of(op).root != root) {
            return Err((Some(index), format!("{:?} edits another root than this clip", path_of(op).root)));
        }
        let lifted = self.picture_v2().map_err(|why| (None, why))?;

        // Each op on the copy, in turn, with the copy as it stood before it:
        // where a structural op lands in the list is read from the chain.
        let mut docs = Documents::default();
        docs.clips.insert(self.id.0, lifted.picture.clone());
        // The grade is written back from a copy that took the grade's ops
        // alone: the full copy also holds effect nodes the batch added,
        // which have no place in the stored list yet.
        let mut grade_docs = Documents::default();
        grade_docs.clips.insert(self.id.0, lifted.picture.clone());
        let mut steps = Vec::with_capacity(ops.len());
        let mut grade = false;
        let mut pending = Vec::new();
        for (index, op) in ops.iter().enumerate() {
            let before = docs.clips[&self.id.0].clone();
            let t = target(&before, op).map_err(|why| (Some(index), why))?;
            if is_structural(op) {
                if let Some(id) = read_by_id(&before) {
                    return Err((Some(index), format!("{id} is read by id elsewhere in the picture, so it can't move until effects keep their ids")));
                }
            }
            // The engine's batch rule: a node added beside waits for its
            // inputs until the batch ends, then the picture is checked whole.
            if let GraphOp::AddNodeBeside { node, .. } = op {
                pending.push(node.id);
            }
            apply_in_batch(&mut docs, op, &RegistryCatalog, &pending).map_err(|why| (Some(index), why))?;
            if let Target::Grade = t {
                apply_in_batch(&mut grade_docs, op, &RegistryCatalog, &pending).map_err(|why| (Some(index), why))?;
                grade = true;
            }
            steps.push((t, docs.clips[&self.id.0].clone()));
        }
        check_batch_end(&docs, root, &RegistryCatalog).map_err(|error| {
            let index = error.unwired.and_then(|(id, _)| ops.iter().position(|op|
                matches!(op, GraphOp::AddNodeBeside { node, .. } if node.id == id)));
            (index, error.why)
        })?;

        // Written to the fields; a write that fails puts them back.
        let before = self.picture.clone();
        let mut order = list_order(&lifted.picture, self.builtin_effects().len());
        let written = (|| {
            for (index, (op, (t, post))) in ops.iter().zip(&steps).enumerate() {
                if let Target::Effect = t {
                    self.write_effect_op(op, post, &mut order).map_err(|why| (Some(index), why))?;
                }
            }
            if grade {
                self.write_grade(&grade_docs.clips[&self.id.0]).map_err(|why| (None, why))?;
            }
            Ok::<(), (Option<usize>, String)>(())
        })();
        if let Err(e) = written {
            self.picture = before;
            return Err(e);
        }
        Ok(PictureEdit { before, after: self.picture.clone() })
    }

    /// Put the clip's picture back as an edit left it (undo, redo).
    pub fn restore_picture(&mut self, fields: &ClipPictureFields) {
        self.picture = fields.clone();
    }

    /// The grade as the edited copy holds it.
    fn write_grade(&mut self, edited: &ClipPicture) -> Result<(), String> {
        let record = klipp_graph_lift::delift::clip(edited)?;
        let v1 = klipp_graph_lift::to_v1::clip(&serde_json::to_value(&record).map_err(|e| e.to_string())?)?;
        let graph: klipp_nodes::NodeGraph = serde_json::from_value(v1.get("node_graph").cloned().unwrap_or_default()).map_err(|e| format!("the edited grade: {e}"))?;
        self.set_grade(graph);
        Ok(())
    }

    /// One op on a built-in effect entry, through the effect-list mutators.
    fn write_effect_op(&mut self, op: &GraphOp, post: &ClipPicture, order: &mut Vec<Option<NodeId>>) -> Result<(), String> {
        let named = path_of(op).node().ok_or("an empty path names no node")?;
        let index = |order: &[Option<NodeId>], id: NodeId| order.iter().position(|o| *o == Some(id)).ok_or_else(|| format!("{id} isn't a built-in effect of the clip"));
        let id_at = |clip: &mut Clip, i: usize| clip.effect_id_at(i).ok_or_else(|| format!("no effect at {i}"));
        match op {
            GraphOp::SetParam { key, value, .. } => {
                let i = index(order, named)?;
                let mut v = serde_json::to_value(&self.builtin_effects()[i]).map_err(|e| e.to_string())?;
                v[key.as_str()] = stored(value)?;
                let effect: klipp_effects::BuiltinEffect = serde_json::from_value(v).map_err(|_| format!("{key} doesn't take {value:?}"))?;
                let id = id_at(self, i)?;
                self.replace_effect(id, effect);
            }
            GraphOp::SetPayload { payload, .. } => {
                let effect: klipp_effects::BuiltinEffect = serde_json::from_value(payload.clone()).map_err(|e| format!("the effect: {e}"))?;
                let id = id_at(self, index(order, named)?)?;
                self.replace_effect(id, effect);
            }
            GraphOp::SetEnabled { enabled, .. } => {
                let id = id_at(self, index(order, named)?)?;
                self.set_effect_enabled(id, *enabled);
            }
            GraphOp::AddNode { node: new, .. } => {
                let payload = new.legacy.as_ref().map(|l| l.payload.clone()).ok_or("a new effect carries its entry")?;
                let effect: klipp_effects::BuiltinEffect = serde_json::from_value(payload).map_err(|e| format!("the new effect: {e}"))?;
                let at = list_place(post, order, new.id).ok_or("an effect goes after the grade, the display transform and the plug-ins")?;
                let id = self.insert_effect(at, effect).ok_or("the list can't take an effect there")?;
                order.insert(at, Some(new.id));
                for (key, anim) in &new.anim {
                    self.write_anim(id, tag_of(new), key.as_str(), Some(anim))?;
                }
                if !new.enabled {
                    self.set_effect_enabled(id, false);
                }
            }
            GraphOp::RemoveNode { .. } => {
                let i = index(order, named)?;
                let id = id_at(self, i)?;
                self.remove_effect(id);
                order.remove(i);
            }
            GraphOp::MoveAfter { .. } => {
                let from = index(order, named)?;
                let mut rest = order.clone();
                rest.remove(from);
                let to = list_place(post, &rest, named).ok_or("an effect goes after the grade, the display transform and the plug-ins")?;
                if to != from {
                    let id = id_at(self, from)?;
                    if !self.move_effect(id, to) {
                        return Err("the list can't move the effect there".into());
                    }
                    let moved = order.remove(from);
                    order.insert(to, moved);
                }
            }
            GraphOp::AddKeyAt { key, .. } | GraphOp::RemoveKeyAt { key, .. } | GraphOp::SetAnim { key, .. } => {
                let n = node(post, named).ok_or("the effect's node")?;
                let id = id_at(self, index(order, named)?)?;
                self.write_anim(id, tag_of(n), key.as_str(), n.anim.get(key))?;
            }
            _ => return Err("not an effect edit".into()),
        }
        Ok(())
    }

    /// An effect parameter's animation as `anim` has it: its keys and its
    /// modifier stack, each taken off when absent. Keyed by the name the
    /// effect animates it under (a ChromaKey's `soft_edge` as `spill`).
    fn write_anim(&mut self, id: crate::EffectId, tag: &str, key: &str, anim: Option<&klipp_graph_format::Animation>) -> Result<(), String> {
        let name = klipp_graph_lift::lift::keyframe_name(tag, key);
        let keys = anim.map(|a| a.keys.iter().map(key_of).collect::<Result<Vec<_>, _>>()).transpose()?.unwrap_or_default();
        if keys.is_empty() {
            self.remove_effect_keyframes(id, name);
        } else {
            self.edit_effect_keyframes_of(id, name, |v| *v = keys);
        }
        match anim.and_then(|a| a.modifiers.as_ref()) {
            Some(m) => {
                let stack: klipp_core::ModifierStack = serde_json::from_value(m.clone()).map_err(|e| format!("a modifier stack: {e}"))?;
                self.edit_effect_modifiers(id, name, |s| *s = stack);
            }
            None => {
                self.remove_effect_modifiers(id, name);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{MediaId, RationalTime};

    /// A structural op on a clip where something reads an effect by id is
    /// refused, never renumbered under the reference (decision §10, T2).
    /// A clip stored as fields can't hold such a reference, so the check is
    /// run on a lifted picture given one, each kind in turn.
    #[test]
    fn an_effect_read_by_id_is_found_whatever_reads_it() {
        let t = |s: f64| RationalTime::from_secs_f64(s, 24);
        let mut c = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
        c.push_effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 3.0 });
        c.push_effect(klipp_effects::BuiltinEffect::Sharpen { amount: 0.5 });
        let p = c.picture_v2().unwrap().picture.clone();
        assert_eq!(read_by_id(&p), None, "a plain chain reads nothing by id");
        let blur = p.graph.nodes.iter().find(|n| n.type_id.as_str() == types::LEGACY_EFFECT).unwrap().id;

        let mut linked = p.clone();
        linked.graph.nodes.iter_mut().find(|n| n.id == blur).unwrap().wireless.output_link = Some("matte".into());
        assert_eq!(read_by_id(&linked), Some(blur), "a wireless link");

        let mut masked = p.clone();
        let reader = masked.graph.nodes.iter().find(|n| n.type_id.as_str() == types::LEGACY_EFFECT && n.id != blur).unwrap().id;
        masked.graph.edges.push(klipp_graph_format::Edge { from: (blur, "out".into()), to: (reader, "mask".into()) });
        assert_eq!(read_by_id(&masked), Some(blur), "a matte into a mask");
    }

    /// The writers' routing rule is the lift's: every registered effect
    /// takes graph ops exactly when it lifts to a plain effect node.
    #[test]
    fn the_routing_rule_is_the_lifts_for_every_effect() {
        let t = |s: f64| RationalTime::from_secs_f64(s, 24);
        for ty in klipp_effects::registry::effect_types() {
            let sample = (ty.sample)();
            let mut c = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
            c.push_effect(sample.clone());
            let p = c.picture_v2().unwrap().picture.clone();
            let plain = p.graph.nodes.iter().any(|n| n.type_id.as_str() == types::LEGACY_EFFECT);
            assert_eq!(takes_picture_ops(&sample), plain, "{}", ty.tag);
        }
    }

    /// Where an effect lands is read along the chain's edges, not from the
    /// order the node list holds: a shuffled list gives the same place.
    #[test]
    fn a_list_place_follows_the_edges_not_the_node_order() {
        let t = |s: f64| RationalTime::from_secs_f64(s, 24);
        let mut c = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
        for amount in [0.25, 0.5, 0.75] {
            c.push_effect(klipp_effects::BuiltinEffect::Sharpen { amount });
        }
        let p = c.picture_v2().unwrap().picture.clone();
        let order = list_order(&p, 3);
        let ids: Vec<NodeId> = order.iter().flatten().copied().collect();
        let places: Vec<_> = ids.iter().map(|id| list_place(&p, &order, *id)).collect();
        assert_eq!(places, [Some(1), Some(2), Some(3)]);
        let mut shuffled = p.clone();
        shuffled.graph.nodes.reverse();
        assert_eq!(ids.iter().map(|id| list_place(&shuffled, &order, *id)).collect::<Vec<_>>(), places);
    }
}
