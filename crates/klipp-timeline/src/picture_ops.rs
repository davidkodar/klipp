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
//!
//! A batch holding an op the fields can't (an effect's mix or mask, a
//! published grade port; `picture_only`) makes
//! the clip picture-truth (M2 step 9c): the edited copy becomes its
//! picture, and the rest of the batch is written to its fields as above,
//! which are then the picture's view. On a picture-truth clip every batch
//! goes that way.

use crate::{Clip, ClipPictureFields, GradeAccess, LiftedPicture, RegistryCatalog};
use klipp_graph_format::lifted::{types as lt, ClipPicture};
use klipp_graph_format::path::Root;
use klipp_graph_format::{types, Node, NodeId, ParamValue};
use klipp_graph_lift::ops::{apply_in_batch, check_batch_end, GraphOp};
use klipp_graph_lift::paths::Documents;
use klipp_effects::BuiltinEffect;
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
        | GraphOp::SetAnnotations { path, .. }
        | GraphOp::AddKeyAt { path, .. }
        | GraphOp::RemoveKeyAt { path, .. }
        | GraphOp::SetAnim { path, .. }
        | GraphOp::SetPayload { path, .. }
        | GraphOp::Group { path, .. }
        | GraphOp::Ungroup { path } => path,
        GraphOp::Connect { to, .. } | GraphOp::Disconnect { to, .. } => to,
        GraphOp::PublishOutput { group, .. } | GraphOp::Unpublish { group, .. } => group,
    }
}

/// Where an op lands on the clip's fields.
enum Target {
    /// The clip's placement or opacity, set through its PLACEMENT node (M3
    /// step 13): the fields that place it.
    Placement,
    /// Inside the grade group: the grade is written back whole.
    Grade,
    /// A built-in effect entry, by its node.
    Effect,
}

fn node(p: &ClipPicture, id: NodeId) -> Option<&Node> {
    p.graph.nodes.iter().find(|n| n.id == id)
}

/// Whether the fields can't hold `op` at all, so on a picture-truth clip
/// (or to make one) it goes to the picture alone: an effect's or a grade
/// correction's mix or mask channel, an edge into an effect's mask, a port the grade group
/// publishes (for a mask inside the grade to reach an effect), and a node
/// beside the clip's chain (a window, a key) with anything done to it.
fn picture_only(p: &ClipPicture, op: &GraphOp) -> bool {
    // The clip's placement and opacity are fields (M3 step 13).
    if klipp_graph_lift::clip_properties::property_of(p, op).is_some() {
        return false;
    }
    let path = path_of(op);
    let Some(first) = path.nodes.first().and_then(|id| node(p, *id)) else {
        // A node this batch adds beside the chain: only the picture has it.
        return path.nodes.len() == 1;
    };
    let beside = !chain(p).iter().any(|n| n.id == first.id);
    // A reroute (#1746): only the picture has one, on the chain or off it.
    // A grade compound, and anything inside one: the picture's alone too.
    let picture_alone = |n: &Node| is_reroute(n) || klipp_graph_lift::grade_compound::is_compound(n);
    if picture_alone(first) || matches!(op, GraphOp::AddNode { node, .. } if picture_alone(node)) {
        return true;
    }
    match op {
        GraphOp::PublishOutput { .. } | GraphOp::Unpublish { .. } => first.type_id.as_str() == lt::GROUP && path.nodes.len() == 1,
        // A grade correction's mix or mask channel (M3 step 17a): the
        // fields' grade has no place for either.
        GraphOp::SetMix { .. } | GraphOp::SetMaskChannel { .. } if first.type_id.as_str() == lt::GROUP => path.nodes.len() == 2,
        _ if path.nodes.len() != 1 => false,
        _ if beside => true,
        // Root canvas metadata has no legacy field representation.
        GraphOp::SetNodeMeta { .. } | GraphOp::SetAnnotations { .. } | GraphOp::AddNodeBeside { .. } => true,
        GraphOp::SetMix { .. } | GraphOp::SetMaskChannel { .. } => first.type_id.as_str() == types::LEGACY_EFFECT,
        // A mask, or a merge's picture (#1746): wired from beside the chain.
        GraphOp::Connect { to_port, .. } => first.type_id.as_str() == types::LEGACY_EFFECT && matches!(to_port.as_str(), "mask" | "fg"),
        GraphOp::Disconnect { port, .. } => first.type_id.as_str() == types::LEGACY_EFFECT && matches!(port.as_str(), "mask" | "fg"),
        _ => false,
    }
}

/// Whether `n` is a reroute (#1746): a point a wire is routed through,
/// passing on what it reads.
fn is_reroute(n: &Node) -> bool {
    klipp_graph_lift::structure::kind_of(n) == "Reroute"
}

/// What `id` (at the picture's top) gives through any reroutes (#1746): the
/// node that makes it.
fn through_reroutes(p: &ClipPicture, mut id: NodeId) -> Option<&Node> {
    for _ in 0..=p.graph.nodes.len() {
        let n = node(p, id)?;
        if !is_reroute(n) {
            return Some(n);
        }
        id = p.graph.edges.iter().find(|e| e.to.0 == n.id)?.from.0;
    }
    None
}

/// A grade compound on the chain before the display step, and why it
/// can't stand there (#1746): it is run after the display step only, so
/// there it would never be drawn. The clip's own grade is the one before.
fn compound_before_display(p: &ClipPicture) -> Option<(NodeId, String)> {
    let chain = chain(p);
    let display = chain.iter().position(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap_or(chain.len());
    let early = chain[..display].iter().find(|n| klipp_graph_lift::grade_compound::is_compound(n))?;
    let name = if early.label.is_empty() { early.id.to_string() } else { format!("{} ({})", early.label, early.id) };
    Some((early.id, format!("the grade {name} goes after the display step, where a grade in the chain is drawn; before it, the clip's own grade is the one to use")))
}

/// Why a node beside the clip's chain can't be wired as `op` wires it, if
/// it can't: its picture comes from the clip's source or from what the
/// grade made of it, which the renderer gives it; any other source is
/// refused here, naming the node and the port, rather than rendered
/// rewired.
fn side_wiring_refusal(p: &ClipPicture, op: &GraphOp) -> Option<String> {
    let GraphOp::Connect { from, from_port, to, to_port } = op else { return None };
    let target = to.nodes.last().and_then(|id| node(p, *id))?;
    let chain = chain(p);
    let in_chain = |id: NodeId| chain.iter().any(|n| n.id == id);
    if to.nodes.len() != 1 || in_chain(target.id) {
        return None;
    }
    let source = from.nodes.last().and_then(|id| through_reroutes(p, *id));
    // The clip's source, its grade's picture, or another node beside the
    // chain (#2758): a branch of effects whose end a merge lays over.
    // Never the chain past the grade, nor the node itself.
    let fed_by_the_clip = from.nodes.len() == 1
        && from_port.as_str() == "out"
        && source.is_some_and(|n| matches!(n.type_id.as_str(), lt::SOURCE | lt::GROUP) || (!in_chain(n.id) && n.id != target.id));
    let refusal = |reader: NodeId, port: &str| {
        format!(
            "{reader}'s {port} can read the clip's source, its grade's picture or a node beside the chain; {} isn't one it can be given",
            source.map_or_else(|| "that".to_owned(), |n| format!("{} {}", n.id, from_port.as_str())),
        )
    };
    // A reroute routes what it takes on to what reads it (#1746): each node
    // beside the chain it feeds, through further reroutes, is checked as if
    // wired here.
    if is_reroute(target) {
        let mut routes = vec![target.id];
        let mut seen = Vec::new();
        while let Some(r) = routes.pop() {
            if seen.contains(&r) || seen.len() > p.graph.nodes.len() {
                continue;
            }
            seen.push(r);
            for e in p.graph.edges.iter().filter(|e| e.from.0 == r) {
                let Some(reader) = node(p, e.to.0).filter(|n| !in_chain(n.id)) else { continue };
                match is_reroute(reader) {
                    true => routes.push(reader.id),
                    false if !fed_by_the_clip => return Some(refusal(reader.id, e.to.1.as_str())),
                    false => {}
                }
            }
        }
        return None;
    }
    (!fed_by_the_clip).then(|| refusal(target.id, to_port.as_str()))
}

/// Where `op` lands, or why it can't be written to the fields.
fn target(p: &ClipPicture, op: &GraphOp) -> Result<Target, String> {
    // The clip's placement or opacity, by its placement node's stable
    // address or its id (M3 step 13).
    if klipp_graph_lift::clip_properties::property_of(p, op).is_some() {
        return Ok(Target::Placement);
    }
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
        // Only a correction at the grade's top level keeps a mix or mask
        // channel (M3 step 17a, `picture_only`); one inside a compound
        // would be written to fields that drop it.
        if matches!(op, GraphOp::SetMix { .. } | GraphOp::SetMaskChannel { .. }) {
            return Err("a correction inside a compound doesn't keep a mix or mask channel yet".into());
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
        GraphOp::SetNodeMeta { .. } | GraphOp::SetAnnotations { .. } => Err("an effect's name and place aren't kept until effects become nodes".into()),
        GraphOp::AddNodeBeside { .. } => Err("a node beside the clip's own chain waits for clips whose picture is stored as their revision-2 picture".into()),
        GraphOp::SetMix { .. } => Err("an effect's mix isn't kept until effects become nodes".into()),
        _ => Err("an effect in the clip's chain has nothing to wire".into()),
    }
}

/// The Transform a placement is fixed by when it moves nothing: what a
/// placement made fixed, or moved from automatic, is put in the list as.
const IDENTITY_TRANSFORM: BuiltinEffect = BuiltinEffect::Transform {
    position_x: 0.0,
    position_y: 0.0,
    scale_x: 1.0,
    scale_y: 1.0,
    rotation: 0.0,
    anchor_x: 0.5,
    anchor_y: 0.5,
    rotation_x: 0.0,
    rotation_y: 0.0,
    perspective: 0.0,
    focal_ref: None,
};

/// Whether `p` moves the clip at all: any value other than the identity
/// (an unset value is the identity). An unmoved placement made automatic
/// needs no picture of its own.
fn moves(p: &klipp_graph_format::lifted::PlacementProps) -> bool {
    let off = |v: Option<f32>, identity: f32| v.is_some_and(|v| v != identity);
    off(p.pos_x, 0.0)
        || off(p.pos_y, 0.0)
        || off(p.scale, 1.0)
        || off(p.aspect, 1.0)
        || off(p.rotation, 0.0)
        || off(p.anchor_x, 0.5)
        || off(p.anchor_y, 0.5)
        || off(p.tilt_x, 0.0)
        || off(p.tilt_y, 0.0)
        || off(p.perspective, 0.0)
        || p.focal_ref.is_some()
        || p.flip_x
        || p.flip_y
}

/// The Transform entry that holds `p`, a placement's values, fixed in the
/// list: what making a placement fixed, or moving it, puts there, so its
/// move stays (#1394).
fn transform_of(p: &klipp_graph_format::lifted::PlacementProps) -> BuiltinEffect {
    // A flip is a negative scale in a Transform (#2705).
    let (scale_x, scale_y) = p.scales();
    BuiltinEffect::Transform {
        position_x: p.pos_x.unwrap_or(0.0),
        position_y: p.pos_y.unwrap_or(0.0),
        scale_x,
        scale_y,
        rotation: p.rotation.unwrap_or(0.0),
        anchor_x: p.anchor_x.unwrap_or(0.5),
        anchor_y: p.anchor_y.unwrap_or(0.5),
        rotation_x: p.tilt_x.unwrap_or(0.0),
        rotation_y: p.tilt_y.unwrap_or(0.0),
        perspective: p.perspective.unwrap_or(0.0),
        focal_ref: p.focal_ref.clone().and_then(|f| serde_json::from_value(f).ok()),
    }
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
        ParamValue::Float64(_)
        | ParamValue::Point2F64(_)
        | ParamValue::Point3F64(_)
        | ParamValue::Int2(_)
        | ParamValue::Int3(_)
        | ParamValue::Choice(_)
        | ParamValue::ColorF64 { .. } => return Err("that value is a plug-in's, set through its own editor".into()),
    })
}

fn tag_of(n: &Node) -> &str {
    klipp_graph_lift::effect_params::tag(n).unwrap_or("")
}

fn key_of(v: &Value) -> Result<klipp_core::Keyframe, String> {
    serde_json::from_value(v.clone()).map_err(|e| format!("a key: {e}"))
}

/// Whether `effect` is an entry graph ops write: a plain effect node, not
/// one of the four the clip places itself. The writers route by it, with
/// the conversion's own rule (`names::effect_node_type`), so they and the
/// refusal here can't disagree.
pub fn takes_picture_ops(effect: &klipp_effects::BuiltinEffect) -> bool {
    let tag = serde_json::to_value(effect).ok().and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_owned)).unwrap_or_default();
    klipp_graph_lift::names::effect_node_type(&tag) == types::LEGACY_EFFECT
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

/// An edit of a clip's OFX plug-in list, by place (M4 step 12b-0).
#[derive(Clone, Copy, Debug)]
pub enum OfxEdit<'a> {
    /// Put a plug-in at the end of the list.
    Add(&'a klipp_plugin::AppliedEffect),
    Remove(usize),
    SetEnabled(usize, bool),
    /// The plug-in at a place, replaced whole.
    Update(usize, &'a klipp_plugin::AppliedEffect),
    /// Move the plug-in at `from` to place `to` (counted after it is taken
    /// out).
    Move { from: usize, to: usize },
    /// One parameter value of the plug-in at a place.
    SetParam(usize, &'a str, &'a klipp_core::OfxValue),
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
        // A batch goes through the clip's picture's record, settled by the
        // lift (M4 step 12b-0): one write path for the list, whose
        // renumbering keeps the clip's orphan keys orphans. A
        // picture-truth clip's plain edits stay on the graph ops below (its
        // record is its picture's view, which a lift would strip); its
        // placed ones are refused there, by name.
        let placed_at = |c: &Self, i: usize| c.builtin_effects().get(i).is_some_and(|e| !takes_picture_ops(e));
        let touches_placed = edits.iter().any(|e| match *e {
            EffectEdit::Add(x) => !takes_picture_ops(x),
            EffectEdit::Update(i, x) => !takes_picture_ops(x) || placed_at(self, i),
            EffectEdit::Remove(i) | EffectEdit::SetEnabled(i, _) => placed_at(self, i),
            EffectEdit::Move { from, .. } => placed_at(self, from),
        });
        if touches_placed || !self.is_picture_truth() {
            return Some(self.placed_edit(edits));
        }
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

    /// `edits` made through the clip's picture's record, the roles they
    /// leave (the clip's opacity, its placement, the chain's head) settled
    /// by the lift, as one edit (M4 step 12b-0): each edit named by the id
    /// of the entry it touches, then the list written from the settled
    /// record. A refusal (a picture-truth clip, a lift that would lose part
    /// of the clip) leaves the clip as it was.
    fn placed_edit(&mut self, edits: &[EffectEdit]) -> Result<PictureEdit, String> {
        use klipp_graph_lift::placed::PlacedEdit;
        let lifted = self.picture_v2()?;
        let kind = klipp_graph_lift::lift::ClipKind::from_json(Some(&serde_json::json!({ "type": crate::picture_fields::lift_kind_tag(&self.kind) })))?;
        let ctx = klipp_graph_lift::lift::LiftCtx { clip_id: self.id.0, kind };
        let mut ids: Vec<NodeId> = self.effect_ids_for_picture().iter().map(|id| id.as_uuid()).collect();
        let at = |ids: &[NodeId], i: usize| ids.get(i).copied().ok_or_else(|| format!("no effect at {i}"));
        let entry = |e: &BuiltinEffect| serde_json::to_value(e).map_err(|e| e.to_string());
        let mut placed = Vec::with_capacity(edits.len());
        for edit in edits {
            match *edit {
                EffectEdit::Add(e) => {
                    let id = crate::EffectId::fresh().as_uuid();
                    placed.push(PlacedEdit::Add { id, entry: entry(e)?, after: ids.last().copied() });
                    ids.push(id);
                }
                EffectEdit::Remove(i) => {
                    placed.push(PlacedEdit::Remove(at(&ids, i)?));
                    ids.remove(i);
                }
                EffectEdit::SetEnabled(i, on) => placed.push(PlacedEdit::SetEnabled(at(&ids, i)?, on)),
                EffectEdit::Update(i, e) => placed.push(PlacedEdit::SetPayload(at(&ids, i)?, entry(e)?)),
                EffectEdit::Move { from, to } => {
                    let id = at(&ids, from)?;
                    ids.remove(from);
                    let after = match to {
                        0 => None,
                        _ => Some(at(&ids, to - 1)?),
                    };
                    ids.insert(to.min(ids.len()), id);
                    placed.push(PlacedEdit::Move { node: id, after });
                }
            }
        }
        let (record, _settled) = klipp_graph_lift::placed::edit(&lifted.picture, &ctx, &placed)?;
        let before = self.picture.clone();
        if let Err(why) = self.write_list_from_record(&record) {
            self.picture = before;
            return Err(why);
        }
        Ok(PictureEdit { before, after: self.picture.clone() })
    }

    /// `edits` of the clip's OFX plug-in list made through its picture's
    /// record, settled by the lift, as one edit (M4 step 12b-0): the list
    /// written back from the record. A picture-truth clip is refused, by
    /// name. The plug-ins keep no ids of their own yet (#1500): their nodes
    /// are named by their places, so a moved plug-in's node is named anew.
    pub fn apply_ofx_edits(&mut self, edits: &[OfxEdit]) -> Result<PictureEdit, String> {
        use klipp_graph_lift::placed::PlacedEdit;
        let lifted = self.picture_v2()?;
        let kind = klipp_graph_lift::lift::ClipKind::from_json(Some(&serde_json::json!({ "type": crate::picture_fields::lift_kind_tag(&self.kind) })))?;
        let ctx = klipp_graph_lift::lift::LiftCtx { clip_id: self.id.0, kind };
        let mut ids: Vec<NodeId> = (0..self.ofx_effects().len()).map(|i| self.effect_node_at(true, i).ok_or_else(|| format!("no plug-in at {i}"))).collect::<Result<_, _>>()?;
        let at = |ids: &[NodeId], i: usize| ids.get(i).copied().ok_or_else(|| format!("no plug-in at {i}"));
        let entry = |e: &klipp_plugin::AppliedEffect| serde_json::to_value(crate::picture_v1::EffectJson::of(e)).map_err(|e| e.to_string());
        // The entries' own ids (#1500), by place, for an update to keep.
        let mut entry_ids: Vec<Option<String>> = self.picture_ofx()?.into_iter().map(|o| o.entry.id).collect();
        let mut placed = Vec::with_capacity(edits.len());
        for edit in edits {
            match *edit {
                // A new entry takes an id of its own, the node's, whatever
                // it came with: a pasted copy never shares its source's.
                OfxEdit::Add(e) => {
                    let id = uuid::Uuid::new_v4();
                    let added = klipp_plugin::AppliedEffect { id: Some(id.to_string()), ..e.clone() };
                    placed.push(PlacedEdit::AddOfx { id, entry: entry(&added)?, after: ids.last().copied() });
                    ids.push(id);
                    entry_ids.push(added.id);
                }
                OfxEdit::Remove(i) => {
                    placed.push(PlacedEdit::Remove(at(&ids, i)?));
                    ids.remove(i);
                    entry_ids.remove(i);
                }
                OfxEdit::SetEnabled(i, on) => placed.push(PlacedEdit::SetEnabled(at(&ids, i)?, on)),
                // Replaced whole, the place keeps its id unless the new
                // entry brings one.
                OfxEdit::Update(i, e) => {
                    let kept = klipp_plugin::AppliedEffect { id: e.id.clone().or_else(|| entry_ids.get(i).cloned().flatten()), ..e.clone() };
                    placed.push(PlacedEdit::SetPayload(at(&ids, i)?, entry(&kept)?));
                    if let Some(slot) = entry_ids.get_mut(i) {
                        *slot = kept.id;
                    }
                }
                OfxEdit::Move { from, to } => {
                    let id = at(&ids, from)?;
                    ids.remove(from);
                    let moved = entry_ids.remove(from);
                    entry_ids.insert(to.min(entry_ids.len()), moved);
                    let after = match to {
                        0 => None,
                        _ => Some(at(&ids, to - 1)?),
                    };
                    ids.insert(to.min(ids.len()), id);
                    placed.push(PlacedEdit::Move { node: id, after });
                }
                OfxEdit::SetParam(i, name, value) => {
                    // The value as the file writes it: through the entry's own
                    // mapping, one value long.
                    let one = klipp_plugin::AppliedEffect { param_values: [(name.to_owned(), value.clone())].into(), ..Default::default() };
                    let written = entry(&one)?;
                    let value = written.get("param_values").and_then(|v| v.get(name)).cloned().ok_or("the value has no stored form")?;
                    placed.push(PlacedEdit::SetOfxParam { node: at(&ids, i)?, name: name.to_owned(), value });
                }
            }
        }
        let (record, _settled) = klipp_graph_lift::placed::edit(&lifted.picture, &ctx, &placed)?;
        let v1 = klipp_graph_lift::to_v1::clip(&serde_json::to_value(&record).map_err(|e| e.to_string())?)?;
        let stored: Vec<crate::picture_v1::EffectJson> = serde_json::from_value(v1.get("effects").cloned().filter(|v| !v.is_null()).unwrap_or_else(|| serde_json::json!([]))).map_err(|e| format!("the edited plug-ins: {e}"))?;
        let before = self.picture.clone();
        self.set_ofx_effects(stored.iter().map(|e| e.to_effect()).collect());
        Ok(PictureEdit { before, after: self.picture.clone() })
    }

    /// The built-in list, its flags, ids, keys and modifiers as `record`
    /// holds them: each entry under its node's id, in slot order.
    fn write_list_from_record(&mut self, record: &klipp_graph_format::ClipRecordV2) -> Result<(), String> {
        let v1 = klipp_graph_lift::to_v1::clip(&serde_json::to_value(record).map_err(|e| e.to_string())?)?;
        // A list the record left empty is written as nothing at all.
        let field = |name: &str| v1.get(name).cloned().filter(|v| !v.is_null()).unwrap_or_else(|| serde_json::json!([]));
        let effects: Vec<BuiltinEffect> = serde_json::from_value(field("builtin_effects")).map_err(|e| format!("the edited list: {e}"))?;
        let enabled: Vec<bool> = serde_json::from_value(field("builtin_effects_enabled")).unwrap_or_default();
        let keyframes: std::collections::HashMap<String, Vec<klipp_core::Keyframe>> = serde_json::from_value(field("effect_keyframes")).unwrap_or_default();
        let modifiers: Vec<(klipp_core::ParamId, klipp_core::ModifierStack)> = v1
            .get("param_modifiers")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| {
                let id = klipp_core::ParamId::from_key(k)?;
                matches!(id, klipp_core::ParamId::Effect { .. }).then_some(())?;
                Some((id, serde_json::from_value(v.clone()).ok()?))
            })
            .collect();
        let mut listed: Vec<&klipp_graph_format::Node> = record.graph.nodes.iter().filter(|n| !matches!(n.type_id.as_str(), types::LEGACY_GRADE | types::LEGACY_OFX)).collect();
        listed.sort_by_key(|n| n.legacy.as_ref().and_then(|l| l.slot).unwrap_or(u32::MAX));
        let ids = listed.iter().map(|n| crate::EffectId::of_node(n.id)).collect();
        self.restore_effects_state(crate::EffectsState::of_picture(effects, enabled, ids, keyframes, modifiers));
        Ok(())
    }

    /// Apply `ops` to the clip's picture as one edit, checked first on a
    /// copy of its lifted picture. A refusal leaves the clip as it was.
    pub fn apply_picture_ops(&mut self, ops: &[GraphOp]) -> Result<PictureEdit, String> {
        self.apply_picture_ops_indexed(ops).map_err(|(_, why)| why)
    }

    /// Apply one atomic batch, retaining the index of an operation-level refusal.
    /// Whole-picture failures have no index unless an unwired node was added here.
    pub fn apply_picture_ops_indexed(&mut self, ops: &[GraphOp]) -> Result<PictureEdit, (Option<usize>, String)> {
        self.apply_picture_ops_renamed(ops).map(|(edit, _)| edit)
    }

    /// [`Self::apply_picture_ops_indexed`], and each built-in effect node id
    /// the batch's picture held that the clip's picture holds under another
    /// id once written back (#1256): the fields keep no effect ids, so the
    /// picture lifted from them names each effect by its list slot. An
    /// effect the batch added, or moved, gets a new id; a reply that names
    /// it by the batch's id would point at nothing. Empty for a batch that
    /// renames nothing.
    pub fn apply_picture_ops_renamed(&mut self, ops: &[GraphOp]) -> Result<(PictureEdit, Vec<(NodeId, NodeId)>), (Option<usize>, String)> {
        if ops.is_empty() {
            return Err((None, "an empty batch has no edit".into()));
        }
        let root = Root::Clip { clip: self.id.0 };
        if let Some((index, op)) = ops.iter().enumerate().find(|(_, op)| path_of(op).root != root) {
            return Err((Some(index), format!("{:?} edits another root than this clip", path_of(op).root)));
        }
        let lifted = self.picture_v2().map_err(|why| (None, why))?;
        if let Some(done) = self.placement_list_edit(&lifted.picture.clone(), ops) {
            return done;
        }
        // Picture-truth already, or made so by an op the fields can't hold
        // (each judged on the copy as the ops before it left it, below).
        let mut truth = self.is_picture_truth();

        // Each op on the copy, in turn, with the copy as it stood before it:
        // where a structural op lands in the list is read from the chain.
        let mut docs = Documents::default();
        docs.clips.insert(self.id.0, lifted.picture.clone());
        // The picture the batch starts from, for its end's check.
        let start = docs.clone();
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
            // An op the fields can't hold goes to the picture alone, and
            // the clip is picture-truth from here on.
            if let Some(why) = side_wiring_refusal(&before, op) {
                return Err((Some(index), why));
            }
            let t = if picture_only(&before, op) {
                truth = true;
                None
            } else {
                Some(target(&before, op).map_err(|why| (Some(index), why))?)
            };
            // The engine's batch rule: a node added beside waits for its
            // inputs until the batch ends, then the picture is checked whole.
            if let GraphOp::AddNodeBeside { node, .. } = op {
                pending.push(node.id);
            }
            apply_in_batch(&mut docs, op, &RegistryCatalog, &pending).map_err(|why| (Some(index), why))?;
            if let Some(Target::Grade) = t {
                apply_in_batch(&mut grade_docs, op, &RegistryCatalog, &pending).map_err(|why| (Some(index), why))?;
                grade = true;
            }
            steps.push((t, docs.clips[&self.id.0].clone()));
        }
        check_batch_end(&docs, &start, root, &RegistryCatalog).map_err(|error| {
            let index = error.unwired.and_then(|(id, _)| ops.iter().position(|op|
                matches!(op, GraphOp::AddNodeBeside { node, .. } if node.id == id)));
            (index, error.why)
        })?;
        if let Some((id, why)) = compound_before_display(&docs.clips[&self.id.0]) {
            let index = ops.iter().position(|op| matches!(op, GraphOp::AddNode { node, .. } if node.id == id) || path_of(op).nodes.first() == Some(&id));
            return Err((index, why));
        }

        // Written to the fields; a write that fails puts them back. On a
        // picture-truth clip the fields are written as the picture's view,
        // with the picture out of the way, and the edited copy is then the
        // picture.
        let before = self.picture.clone();
        self.picture.take_truth();
        let mut order = list_order(&lifted.picture, self.builtin_effects().len());
        let written = (|| {
            for (index, (op, (t, post))) in ops.iter().zip(&steps).enumerate() {
                match t {
                    Some(Target::Effect) => self.write_effect_op(op, post, &mut order).map_err(|why| (Some(index), why))?,
                    // A picture-truth clip's keys are written through to its
                    // fields' lanes too: a later write to its fields lifts
                    // them again, and keys only its picture held would be
                    // lost then. So is a value set where an entry of the
                    // fields holds it; one with none stays its picture's
                    // (the fields can't hold it without an entry the picture
                    // doesn't have), and `merge_truth` carries it.
                    Some(Target::Placement) if !truth || !matches!(op, GraphOp::SetParam { key, .. } if self.placement_entry(key.as_str()).is_none()) => self.write_placement_op(op).map_err(|why| (Some(index), why))?,
                    _ => {}
                }
            }
            if grade {
                let edited = &grade_docs.clips[&self.id.0];
                let edited = if truth { klipp_graph_lift::delift::view_of(edited) } else { edited.clone() };
                self.write_grade(&edited).map_err(|why| (None, why))?;
            }
            if truth {
                let picture = self.truth_from(docs.clips.remove(&self.id.0).expect("the copy")).map_err(|why| (None, why))?;
                self.picture.set_truth(Some(std::sync::Arc::new(picture)));
            }
            Ok::<(), (Option<usize>, String)>(())
        })();
        if let Err(e) = written {
            self.picture = before;
            return Err(e);
        }
        // Each slot's effect, by the id the batch knew it by and the id the
        // written clip's picture gives it. A picture-truth clip keeps the
        // batch's ids, so it renames nothing.
        let renamed = if truth { Vec::new() } else { self.renamed(&order) };
        Ok((PictureEdit { before, after: self.picture.clone() }, renamed))
    }

    /// Each effect the list held at a slot of `order` (by the id the batch
    /// knew it by) that the written clip's picture holds under another id.
    fn renamed(&self, order: &[Option<NodeId>]) -> Vec<(NodeId, NodeId)> {
        let Ok(after) = self.picture_v2() else { return Vec::new() };
        order
            .iter()
            .enumerate()
            .filter_map(|(slot, was)| {
                let was = (*was)?;
                let now = after.picture.graph.nodes.iter().filter(|n| n.type_id.as_str() != types::LEGACY_OFX).find(|n| n.legacy.as_ref().and_then(|l| l.slot) == Some(slot as u32))?.id;
                (now != was).then_some((was, now))
            })
            .collect()
    }

    /// A batch of one op on the clip's own PLACEMENT node that moves it or
    /// sets its mode (M3 DEC-§2), written to the fields that place it: a
    /// fixed placement is the first enabled Transform entry, so a move
    /// moves that entry, as the old tab's reorder did; an automatic one has
    /// none, so moving it, or making it fixed, puts a Transform holding its
    /// values where it goes. Making it automatic takes the entry away; its
    /// lanes stay, and a static move stays too (#1394, picture revision 4):
    /// the fields can't hold an automatic placement that moves, so the clip
    /// becomes picture-truth, keeping it. On a picture-truth clip the
    /// fields are written as its view and the picture is carried over, as
    /// [`Clip::through_truth`] does. `None` for any other batch.
    fn placement_list_edit(&mut self, picture: &ClipPicture, ops: &[GraphOp]) -> Option<Result<(PictureEdit, Vec<(NodeId, NodeId)>), (Option<usize>, String)>> {
        let at_placement = |op: &GraphOp| {
            let path = path_of(op);
            let named = klipp_graph_lift::clip_properties::placement_node(picture).map(|n| n.id);
            path.nodes.len() == 1 && (named == path.node() || path.root_clip().is_some_and(|clip| klipp_graph_lift::clip_properties::placement_id(clip) == path.nodes[0]))
        };
        let moves = |op: &GraphOp| matches!(op, GraphOp::MoveAfter { .. }) || matches!(op, GraphOp::SetParam { key, .. } if key.as_str() == "mode");
        let index = ops.iter().position(|op| moves(op) && at_placement(op))?;
        if ops.len() != 1 {
            return Some(Err((Some(index), "the placement's place or mode is an edit of its own".into())));
        }
        let before = self.picture.clone();
        let keep = picture.placement.clone();
        self.reconcile_effect_ids();
        let old = self.picture.take_truth();
        let mut order = list_order(picture, self.builtin_effects().len());
        let fixed = klipp_graph_lift::clip_properties::placement_node(picture).filter(|n| n.legacy.is_some()).and_then(|n| order.iter().position(|o| *o == Some(n.id)));
        let written = match &ops[0] {
            GraphOp::MoveAfter { after, .. } => self.move_placement(picture, &mut order, fixed, *after, &keep),
            GraphOp::SetParam { value: ParamValue::Enum(m), .. } if *m == lt::PLACEMENT_FIXED => match fixed {
                Some(_) => Err("the placement is fixed already".into()),
                None => {
                    // Where automatic placement puts it now: before the first
                    // enabled effect drawn outside the picture, or at the end,
                    // so the picture doesn't change.
                    let on = self.builtin_effects_enabled().to_vec();
                    let at = self.builtin_effects().iter().enumerate().position(|(i, e)| on.get(i).copied().unwrap_or(true) && takes_picture_ops(e) && klipp_effects::draws_outside_the_picture(e)).unwrap_or(self.builtin_effects().len());
                    self.insert_placement(&mut order, at, &keep)
                }
            },
            GraphOp::SetParam { value: ParamValue::Enum(m), .. } if *m == lt::PLACEMENT_AUTO => match fixed {
                None => Err("the placement is automatic already".into()),
                Some(slot) => self.unfix_placement(&mut order, slot),
            },
            GraphOp::SetParam { value, .. } => Err(format!("the placement's mode is automatic or fixed, not {value:?}")),
            _ => unreachable!("a move or a mode"),
        };
        let settled = written.and_then(|()| self.settle_placement(old.as_deref(), &before, keep));
        Some(match settled {
            Ok(()) => {
                let renamed = if self.is_picture_truth() { Vec::new() } else { self.renamed(&order) };
                Ok((PictureEdit { before, after: self.picture.clone() }, renamed))
            }
            Err(why) => {
                self.picture = before;
                Err((Some(0), why))
            }
        })
    }

    /// After a placement edit written to the fields: a picture-truth clip's
    /// picture carried over from `old` onto them ([`merge_truth`]), and an
    /// automatic placement given `keep`, the move it had, which makes the
    /// clip picture-truth when its fields can't hold it (#1394).
    fn settle_placement(&mut self, old: Option<&LiftedPicture>, before: &ClipPictureFields, keep: klipp_graph_format::lifted::PlacementProps) -> Result<(), String> {
        self.reconcile_effect_ids();
        let fresh = self.lift_picture()?;
        let mut out = match old {
            Some(old) => merge_truth(old, &(before.effect_ids().ids.clone(), self.picture.effect_ids().ids.clone()), fresh, self.placement_entry("opacity").is_some())?,
            None => fresh,
        };
        let auto = klipp_graph_lift::clip_properties::placement_node(&out.picture).is_some_and(|n| n.legacy.is_none());
        if auto && moves(&keep) && out.picture.placement != keep {
            out.picture.placement = keep;
            out = self.truth_from(out.picture)?;
        }
        if out.picture.is_truth() {
            self.picture.set_truth(Some(std::sync::Arc::new(out)));
        }
        Ok(())
    }

    /// The placement moved after `after`: its entry, when it is fixed (at
    /// `fixed` in the list), else a new identity Transform there.
    fn move_placement(&mut self, picture: &ClipPicture, order: &mut Vec<Option<NodeId>>, fixed: Option<usize>, after: NodeId, keep: &klipp_graph_format::lifted::PlacementProps) -> Result<(), String> {
        let moved = klipp_graph_lift::clip_properties::placement_node(picture).map(|n| n.id).ok_or("the clip has no placement")?;
        if after == moved {
            return Err("the placement can't go after itself".into());
        }
        let mut rest = order.clone();
        if let Some(from) = fixed {
            rest.remove(from);
        }
        let chain = chain(picture);
        let at = chain.iter().position(|n| n.id == after).ok_or_else(|| format!("{after} isn't in the clip's chain"))?;
        let later = &chain[at + 1..];
        if later.iter().any(|n| matches!(n.type_id.as_str(), lt::GROUP | lt::SCENE_TO_DISPLAY | types::LEGACY_OFX)) {
            return Err("the placement goes after the grade, the display transform and the plug-ins".into());
        }
        let to = later.iter().filter(|n| n.id != moved).find_map(|n| rest.iter().position(|o| *o == Some(n.id))).unwrap_or(rest.len());
        match fixed {
            Some(from) if from == to => Err("the placement is there already".into()),
            Some(from) => {
                let id = self.effect_id_at(from).ok_or("the placement's entry is gone")?;
                if !self.move_effect(id, to) {
                    return Err("the list can't move the placement there".into());
                }
                let entry = order.remove(from);
                order.insert(to, entry);
                Ok(())
            }
            None => {
                // Where an enabled Transform before `to` would already be
                // the placement, a new one there wouldn't be.
                let on = self.builtin_effects_enabled().to_vec();
                if self.builtin_effects()[..to].iter().enumerate().any(|(i, e)| matches!(e, BuiltinEffect::Transform { .. }) && on.get(i).copied().unwrap_or(true)) {
                    return Err("an enabled Transform before there places the clip".into());
                }
                self.insert_placement(order, to, keep)
            }
        }
    }

    /// A Transform holding the placement's values `keep` put in the list at
    /// `at`: the clip's placement, fixed there with its move.
    fn insert_placement(&mut self, order: &mut Vec<Option<NodeId>>, at: usize, keep: &klipp_graph_format::lifted::PlacementProps) -> Result<(), String> {
        self.insert_effect(at, transform_of(keep)).ok_or("the list can't take the placement there")?;
        order.insert(at, None);
        Ok(())
    }

    /// Why the clip's fixed placement can't be made automatic, if it
    /// can't: its Transform must have no keys or modifiers of its own and
    /// be the only enabled Transform (another would place the clip). A
    /// static move is kept (#1394). `None` too when the placement is
    /// automatic already. What the Placement row's Auto choice is disabled
    /// by.
    pub fn placement_auto_refusal(&self) -> Option<&'static str> {
        let on = |i: usize| self.builtin_effects_enabled().get(i).copied().unwrap_or(true);
        let transforms: Vec<usize> = self.builtin_effects().iter().enumerate().filter(|(i, e)| matches!(e, BuiltinEffect::Transform { .. }) && on(*i)).map(|(i, _)| i).collect();
        let &slot = transforms.first()?;
        let prefix = format!("{slot}:");
        if self.effect_keyframes().keys().any(|k| k.starts_with(&prefix)) || self.param_modifiers().keys().any(|k| matches!(k, klipp_core::ParamId::Effect { effect_index, .. } if *effect_index == slot)) {
            return Some("the placement's Transform has keys or modifiers of its own");
        }
        (transforms.len() > 1).then_some("another enabled Transform would place the clip")
    }

    /// The fixed placement's entry, at `slot`, taken out so the placement
    /// is automatic, unless [`Self::placement_auto_refusal`] says why not.
    fn unfix_placement(&mut self, order: &mut Vec<Option<NodeId>>, slot: usize) -> Result<(), String> {
        if let Some(why) = self.placement_auto_refusal() {
            return Err(why.into());
        }
        let id = self.effect_id_at(slot).ok_or("the placement's entry is gone")?;
        self.remove_effect(id).ok_or("the placement's entry is gone")?;
        order.remove(slot);
        Ok(())
    }

    /// `edited` as this clip's picture-truth picture, its fields already
    /// written as its view: marked as truth, its grade versions' bodies
    /// and its built-in entries' list places taken from a lift of the view,
    /// so an entry the batch added has its place, and a picture whose
    /// chain and fields disagree is refused.
    fn truth_from(&self, mut edited: ClipPicture) -> Result<LiftedPicture, String> {
        let view = self.lift_picture()?;
        let listed = |p: &ClipPicture| -> Vec<NodeId> {
            // A reroute is the picture's alone (#1746): no entry of the fields.
            chain(p).into_iter().filter(|n| n.legacy.is_some() && !is_reroute(n) && !matches!(n.type_id.as_str(), lt::SOURCE | lt::OUTPUT | lt::SCENE_TO_DISPLAY | lt::GROUP | types::LEGACY_OFX)).map(|n| n.id).collect()
        };
        let (ours, theirs) = (listed(&edited), listed(&view.picture));
        if ours.len() != theirs.len() {
            return Err(format!("the picture's chain holds {} entries and its fields {}", ours.len(), theirs.len()));
        }
        for (mine, from) in ours.iter().zip(&theirs) {
            let slot = node(&view.picture, *from).and_then(|n| n.legacy.as_ref()).and_then(|l| l.slot);
            let n = edited.graph.nodes.iter_mut().find(|n| n.id == *mine).expect("in the chain");
            if let Some(l) = n.legacy.as_mut() {
                l.slot = slot;
            }
        }
        if !edited.is_truth() {
            edited.requires.push(klipp_graph_format::lifted::PICTURE_TRUTH_CAPABILITY.into());
        }
        edited.mark_grade_mix();
        Ok(LiftedPicture { picture: edited, versions: view.versions })
    }

    /// Run `f`, a write through the clip's fields: as it is on a clip whose
    /// fields are the truth. On a picture-truth clip this is the guard
    /// (amendment 12): `f` writes the fields as the picture's view, the
    /// fields are lifted again, and the picture becomes that lift in the
    /// picture's own ids with what only the picture holds carried over (an
    /// effect's mix and mask, its mask's wiring, the grade's published
    /// ports, a node beside the chain). A write that takes away what one of
    /// those refers to is refused: the clip is put back as it was, and the
    /// reason, naming the clip and `what`, is kept for
    /// [`take_picture_refusal`] and logged. `f`'s result is returned either
    /// way.
    pub(crate) fn through_truth<R>(&mut self, what: &str, f: impl FnOnce(&mut Clip) -> R) -> R {
        if !self.is_picture_truth() {
            return f(self);
        }
        self.reconcile_effect_ids();
        let before = self.picture.clone();
        let old = self.picture.take_truth().expect("a picture-truth clip");
        let result = f(self);
        self.settle_truth(&old, before, what);
        result
    }

    /// `base` with `picture` as its picture (M4 step 12f): what a blade's
    /// halves are made from. Until the v1 view goes, a picture-truth
    /// `base` takes `picture` as its truth, and any other keeps the fields
    /// it was made with, which its maker cut the same way.
    pub fn from_picture(base: &Clip, picture: klipp_graph_format::lifted::ClipPicture) -> Clip {
        let mut clip = base.clone();
        if let Some(truth) = base.picture.truth() {
            let mut lifted = (**truth).clone();
            lifted.picture = picture;
            clip.picture.set_truth(Some(std::sync::Arc::new(lifted)));
        }
        clip
    }

    /// Take `source`'s picture-truth picture, if it has one, for this clip
    /// made from it field by field (a split's half, a lifted range's piece,
    /// a duplicate): its picture is `source`'s, settled against its own
    /// fields as a write would be, so what only the picture holds isn't
    /// left behind. Nothing for a source whose fields are the truth.
    pub fn take_picture_truth_of(&mut self, source: &Clip) {
        if let Some(truth) = source.picture.truth() {
            self.picture.set_truth(Some(truth.clone()));
            self.through_truth("a copy", |_| ());
        }
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

    /// The place of the enabled entry of the fields that holds the
    /// placement value `key`: the last enabled Opacity for `opacity`, on a
    /// clip and an adjustment layer alike (#1386), the first enabled
    /// Transform for the rest.
    fn placement_entry(&self, key: &str) -> Option<usize> {
        let on = self.builtin_effects_enabled();
        let mut held = self.builtin_effects().iter().enumerate().filter(|(i, e)| on.get(*i).copied().unwrap_or(true) && if key == "opacity" { matches!(e, BuiltinEffect::Opacity { .. }) } else { matches!(e, BuiltinEffect::Transform { .. }) }).map(|(i, _)| i);
        if key == "opacity" { held.next_back() } else { held.next() }
    }

    /// One op on the clip's placement or opacity (M3 step 13), written to
    /// the fields the lift reads them from: a placement value to the first
    /// enabled Transform entry (one added at the list's end, an identity,
    /// when there's none), the opacity to the entry the lift reads as the
    /// clip's (the last enabled Opacity; an adjustment layer's first), and
    /// their keys to the clip's placement and opacity lanes.
    fn write_placement_op(&mut self, op: &GraphOp) -> Result<(), String> {
        use klipp_core::VideoParam as P;
        let (GraphOp::SetParam { key, .. } | GraphOp::AddKeyAt { key, .. } | GraphOp::RemoveKeyAt { key, .. } | GraphOp::SetAnim { key, .. }) = op else {
            return Err("not an op on a clip property".into());
        };
        if let GraphOp::SetParam { value: ParamValue::Float(v), .. } = op {
            let v = *v;
            if key.as_str() == "opacity" {
                let id = match self.placement_entry("opacity") {
                    Some(i) => self.effect_id_at(i).ok_or("the opacity entry is gone")?,
                    None => self.push_effect(BuiltinEffect::Opacity { value: v }),
                };
                self.edit_effect(id, |e| {
                    if let BuiltinEffect::Opacity { value } = e {
                        *value = v;
                    }
                });
                return Ok(());
            }
            if !klipp_graph_lift::clip_properties::PLACEMENT.contains(&key.as_str()) {
                return Err(format!("{key} is not a placement value"));
            }
            let id = match self.placement_entry(key.as_str()) {
                Some(i) => self.effect_id_at(i).ok_or("the transform entry is gone")?,
                None => self.push_effect(IDENTITY_TRANSFORM),
            };
            let name = key.as_str().to_owned();
            self.edit_effect(id, |e| {
                if let BuiltinEffect::Transform { position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y, rotation_x, rotation_y, perspective, .. } = e {
                    // A flip is a scale's sign (#2705): a size keeps it, a
                    // flip sets it.
                    let sign = |s: f32| if s < 0.0 { -1.0 } else { 1.0 };
                    let (fx, fy) = (sign(*scale_x), sign(*scale_y));
                    match name.as_str() {
                        "pos_x" => *position_x = v,
                        "pos_y" => *position_y = v,
                        // The width; the height keeps the aspect under it.
                        "scale" => {
                            let aspect = klipp_graph_lift::names::aspect(Some(scale_x.abs()), Some(scale_y.abs())).unwrap_or(1.0);
                            (*scale_x, *scale_y) = (v * fx, v * aspect * fy);
                        }
                        "scale_y" => *scale_y = v * fy,
                        "flip_x" => *scale_x = scale_x.abs() * if v >= 0.5 { -1.0 } else { 1.0 },
                        "flip_y" => *scale_y = scale_y.abs() * if v >= 0.5 { -1.0 } else { 1.0 },
                        "rotation" => *rotation = v,
                        "anchor_x" => *anchor_x = v,
                        "anchor_y" => *anchor_y = v,
                        "tilt_x" => *rotation_x = v,
                        "tilt_y" => *rotation_y = v,
                        "perspective" => *perspective = v,
                        // Checked against the placement's keys above.
                        _ => {}
                    }
                }
            });
            return Ok(());
        }
        if let GraphOp::SetParam { value, .. } = op {
            return Err(format!("{key} takes a number, not {value:?}"));
        }
        let lane = match key.as_str() {
            "pos_x" => P::PositionX,
            "pos_y" => P::PositionY,
            "scale" => P::Scale,
            "scale_y" => P::ScaleY,
            "rotation" => P::Rotation,
            "tilt_x" => P::TiltX,
            "tilt_y" => P::TiltY,
            "opacity" => P::Opacity,
            other => return Err(format!("{other} doesn't take keyframes")),
        };
        let parse = |k: &Value| serde_json::from_value::<klipp_core::Keyframe>(k.clone()).map_err(|e| format!("a key: {e}"));
        match op {
            GraphOp::AddKeyAt { keyframe, .. } => {
                let k = parse(keyframe)?;
                self.edit_video_param_keyframes(lane, |keys| {
                    keys.retain(|x| (x.time_secs - k.time_secs).abs() > 1e-9);
                    let at = keys.iter().position(|x| x.time_secs > k.time_secs).unwrap_or(keys.len());
                    keys.insert(at, k);
                });
            }
            GraphOp::RemoveKeyAt { time_secs, .. } => {
                let t = *time_secs;
                let found = self.edit_video_param_keyframes(lane, |keys| {
                    let before = keys.len();
                    keys.retain(|x| (x.time_secs - t).abs() > 1e-9);
                    keys.len() != before
                });
                if !found {
                    return Err(format!("{key} has no key at {t} s"));
                }
            }
            GraphOp::SetAnim { anim, .. } => {
                if anim.as_ref().is_some_and(|a| a.modifiers.is_some()) {
                    return Err(format!("{key}'s modifiers are set on its own lane, not with its keys"));
                }
                let keys = anim.iter().flat_map(|a| &a.keys).map(parse).collect::<Result<Vec<_>, _>>()?;
                self.edit_video_param_keyframes(lane, |k| *k = keys);
            }
            _ => {}
        }
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
                // The value written as the picture's node holds it (its
                // map, M3-2), so the list and the node agree on a field
                // keyed by element too.
                use klipp_graph_lift::effect_params as ep;
                if !ep::is_field_key(key) {
                    return Err(format!("{key} is the node's own, not a setting of its effect"));
                }
                let v = serde_json::to_value(&self.builtin_effects()[i]).map_err(|e| e.to_string())?;
                let (tag, mut params) = ep::to_params(&v)?;
                // A keyframe alias's value is the field it stands for (a
                // ChromaKey's `spill` is its `soft_edge`), as in the lift's
                // own writer.
                let key = &klipp_graph_format::ParamKey::from(klipp_graph_lift::names::anim_key(&tag, key.as_str()));
                params.insert(key.clone(), ep::held(&tag, key, &stored(value)?));
                let effect = ep::effect(&tag, &params).map_err(|_| format!("{key} doesn't take {value:?}"))?;
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
                let mut held = (**new).clone();
                klipp_graph_lift::effect_params::added_into_memory(&mut held).map_err(|e| format!("the new effect: {e}"))?;
                if held.legacy.is_none() {
                    return Err("a new effect carries its entry".into());
                }
                let effect = klipp_graph_lift::effect_params::effect_of(&held).map_err(|e| format!("the new effect: {e}"))?;
                let at = list_place(post, order, new.id).ok_or("an effect goes after the grade, the display transform and the plug-ins")?;
                // The entry keeps the id the op gave its node (M3 proposal 6).
                let id = self.insert_effect_with_id(at, effect, crate::EffectId::of_node(new.id)).ok_or("the list can't take an effect there, or holds one by that id")?;
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
                // Read under the key the op stored it under (an alias's own).
                let stored = klipp_graph_lift::names::anim_key(tag_of(n), key.as_str());
                self.write_anim(id, tag_of(n), stored, n.anim.get(&klipp_graph_format::ParamKey::from(stored)))?;
            }
            _ => return Err("not an effect edit".into()),
        }
        Ok(())
    }

    /// An effect parameter's animation as `anim` has it: its keys and its
    /// modifier stack, each taken off when absent. Keyed by the name the
    /// effect animates it under (a ChromaKey's `soft_edge` as `spill`).
    fn write_anim(&mut self, id: crate::EffectId, tag: &str, key: &str, anim: Option<&klipp_graph_format::Animation>) -> Result<(), String> {
        let name = klipp_graph_lift::names::keyframe_name(tag, key);
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
        let c = crate::test_clip::TestClip::of(Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0)))
            .effects([0.25, 0.5, 0.75].map(|amount| klipp_effects::BuiltinEffect::Sharpen { amount }))
            .build();
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

/// A write to a clip's fields, open: dereferences to them. On a
/// picture-truth clip it is the guard for every write that isn't graph ops
/// (amendment 12): when it ends, the fields are lifted again and the
/// picture becomes that lift with what only the picture holds carried over,
/// or the write is refused and the clip put back as it was
/// (`Clip::through_truth` says how). Every writer of a clip's fields
/// takes one ([`Clip::view_mut`]); a write that skips it trips
/// `ClipPictureFields::view_mut`'s check.
pub(crate) struct ViewMut<'a> {
    clip: &'a mut Clip,
    /// On a picture-truth clip: its picture, taken out while the fields are
    /// written, and the clip as it was, to put back on a refusal.
    truth: Option<(std::sync::Arc<LiftedPicture>, ClipPictureFields)>,
}

impl std::ops::Deref for ViewMut<'_> {
    type Target = crate::picture_store::PictureView;
    fn deref(&self) -> &Self::Target {
        self.clip.picture.view()
    }
}

impl std::ops::DerefMut for ViewMut<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.clip.picture.view_mut()
    }
}

impl Drop for ViewMut<'_> {
    fn drop(&mut self) {
        if let Some((old, before)) = self.truth.take() {
            self.clip.settle_truth(&old, before, "a write to its fields");
        }
    }
}

impl Clip {
    /// Open a write to the clip's fields ([`ViewMut`]).
    pub(crate) fn view_mut(&mut self) -> ViewMut<'_> {
        let truth = self.picture.truth().is_some().then(|| {
            self.reconcile_effect_ids();
            let before = self.picture.clone();
            (self.picture.take_truth().expect("a picture-truth clip"), before)
        });
        ViewMut { clip: self, truth }
    }

    /// After a write to a picture-truth clip's fields with its picture
    /// `old` taken out: the fields lifted again, merged as its picture, or
    /// the write refused and the clip put back as `before`.
    fn settle_truth(&mut self, old: &LiftedPicture, before: ClipPictureFields, what: &str) {
        self.reconcile_effect_ids();
        let ids = (before.effect_ids().ids.clone(), self.picture.effect_ids().ids.clone());
        let anchored = self.placement_entry("opacity").is_some();
        match self.lift_picture().and_then(|fresh| merge_truth(old, &ids, fresh, anchored)) {
            Ok(picture) => self.picture.set_truth(Some(std::sync::Arc::new(picture))),
            Err(why) => {
                self.picture = before;
                tracing::warn!("clip {}: {what} refused: {why}", self.id.0);
                // For the person who made the edit: the clip by its name,
                // or its id when it has none.
                let clip = self.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| format!("clip {}", self.id.0));
                refuse_picture_write(format!("{clip}: {what} not done: {why}"));
            }
        }
    }
}

/// Count a refused write through a picture-truth clip's fields and keep
/// why, for [`take_picture_refusal`].
pub(crate) fn refuse_picture_write(why: String) {
    PICTURE_WRITES_REFUSED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    REFUSAL.with(|r| *r.borrow_mut() = Some(why));
}

/// Writes through a picture-truth clip's fields that were refused
/// (`Clip::through_truth`), in this process.
pub static PICTURE_WRITES_REFUSED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

thread_local! {
    static REFUSAL: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Why the last write through a picture-truth clip's fields on this thread
/// was refused, once: the clip was put back as it was. For the caller
/// that made the write to tell the user.
pub fn take_picture_refusal() -> Option<String> {
    REFUSAL.with(|r| r.borrow_mut().take())
}

/// The built-in entries of a picture's chain by list place: each with its
/// node and its kind (the node's type, and for an effect node the effect
/// it holds, so a sharpen is never taken for a blur). Plug-ins are counted
/// apart and left out.
fn entries(p: &ClipPicture) -> Vec<(u32, NodeId, String)> {
    chain(p)
        .into_iter()
        .filter(|n| n.type_id.as_str() != types::LEGACY_OFX)
        .filter_map(|n| Some((n.legacy.as_ref()?.slot?, n.id, format!("{} {}", n.type_id.as_str(), tag_of(n)))))
        .collect()
}

/// `fresh`, a picture-truth clip's fields lifted again after a write, as
/// its picture: in the old picture's ids, with what only the picture holds
/// carried over from `old`. An entry keeps its node when it is the same
/// effect (its `EffectId`, by `ids`: the list's ids before the write and
/// after) and of the same type, wherever the write moved it. Refused,
/// naming it, when the write took away what the picture's own data refers
/// to.
fn merge_truth(old: &LiftedPicture, ids: &(Vec<crate::EffectId>, Vec<crate::EffectId>), fresh: LiftedPicture, anchored: bool) -> Result<LiftedPicture, String> {
    let mut p = fresh.picture;
    let before = entries(&old.picture);
    let (was, now) = ids;
    // A clip given a new id (a split's half, a duplicate) has new effect
    // ids for the same list: the entries are the same, place by place.
    let copied = !was.is_empty() && was.len() == now.len() && was.iter().all(|e| !now.contains(e));
    let mut rename: std::collections::HashMap<NodeId, NodeId> = entries(&p)
        .into_iter()
        .filter_map(|(slot, id, ty)| {
            let was_at = if copied { slot } else { was.iter().position(|e| Some(e) == now.get(slot as usize))? as u32 };
            before.iter().find(|(s, _, t)| *s == was_at && *t == ty).map(|(_, old_id, _)| (id, *old_id))
        })
        .collect();
    // The chain's own nodes, one of each (the source, the grade, the
    // display step, the output), whose ids a lift derives from the clip's.
    for n in chain(&p) {
        if n.legacy.as_ref().is_some_and(|l| l.slot.is_some()) || n.type_id.as_str() == types::LEGACY_OFX {
            continue;
        }
        let of_type = |q: &ClipPicture| chain(q).into_iter().filter(|m| m.type_id == n.type_id && !m.legacy.as_ref().is_some_and(|l| l.slot.is_some())).map(|m| m.id).collect::<Vec<_>>();
        if let ([theirs], [mine]) = (of_type(&old.picture).as_slice(), of_type(&p).as_slice()) {
            rename.insert(*mine, *theirs);
        }
    }
    // An entry the write didn't keep (a new one, or one put where another
    // was) may carry an id the lift derives from its place, which an old
    // node had: it gets its own, so nothing of the old one lands on it.
    let matched: Vec<NodeId> = rename.keys().copied().collect();
    let old_ids: Vec<NodeId> = old.picture.graph.nodes.iter().map(|n| n.id).collect();
    for (_, id, _) in entries(&p) {
        if !matched.contains(&id) && old_ids.contains(&id) {
            rename.insert(id, uuid::Uuid::new_v4());
        }
    }
    rename.retain(|new, old| new != old);
    let renamed = |id: NodeId| rename.get(&id).copied().unwrap_or(id);
    for n in &mut p.graph.nodes {
        n.id = renamed(n.id);
    }
    for e in &mut p.graph.edges {
        e.from.0 = renamed(e.from.0);
        e.to.0 = renamed(e.to.0);
    }
    p.graph.output = renamed(p.graph.output);

    // Root-canvas names and positions belong to the picture, not to the
    // effect list. Match after restoring stable ids so reordering, editing
    // and duplicating a clip keep them on their original nodes.
    for n in &mut p.graph.nodes {
        if let Some(previous) = old.picture.graph.nodes.iter().find(|old| old.id == n.id) {
            n.label.clone_from(&previous.label);
        }
    }

    let old_chain: Vec<NodeId> = chain(&old.picture).iter().map(|n| n.id).collect();
    let label = |n: &Node| if n.label.is_empty() { tag_of(n).to_owned() } else { n.label.clone() };
    // An effect's mix and mask channel.
    let mask_key = klipp_graph_format::ParamKey::from("mask.channel");
    for n in old.picture.graph.nodes.iter().filter(|n| n.type_id.as_str() == types::LEGACY_EFFECT && old_chain.contains(&n.id)) {
        let channel = n.params.get(&mask_key);
        if n.mix.is_none() && channel.is_none() {
            continue;
        }
        let Some(t) = p.graph.nodes.iter_mut().find(|t| t.id == n.id) else {
            return Err(format!("it takes away the {} effect, whose mix or mask only the clip's picture keeps", label(n)));
        };
        t.mix = n.mix;
        if let Some(c) = channel {
            t.params.insert(mask_key.clone(), c.clone());
        }
    }
    // A grade correction's mix and mask channel (M3 step 17a), by the
    // node's id, which the grade keeps. A node the write took out of the
    // grade takes its settings with it: nothing else reads them.
    let grade_settings: Vec<(NodeId, Option<f32>, Option<klipp_graph_format::ParamValue>)> = grade_nodes(&old.picture)
        .filter(|n| n.mix.is_some() || n.params.contains_key(&mask_key))
        .map(|n| (n.id, n.mix, n.params.get(&mask_key).cloned()))
        .collect();
    for (id, mix, channel) in grade_settings {
        if !grade_nodes(&p).any(|t| t.id == id) {
            if grade_nests(&p, id) {
                // Moved into a compound, where nothing keeps them.
                return Err(format!("it puts the grade node {id}, whose mix or mask channel only the clip's picture keeps, inside a compound"));
            }
            continue;
        }
        if let Some(t) = grade_nodes_mut(&mut p).find(|t| t.id == id) {
            t.mix = mix;
            if let Some(c) = channel {
                t.params.insert(mask_key.clone(), c);
            }
        }
    }
    // An automatic placement's own move (#1394): the fields hold none, so
    // while the placement stays automatic it is the picture's.
    let auto = |q: &ClipPicture| klipp_graph_lift::clip_properties::placement_node(q).is_some_and(|n| n.legacy.is_none());
    if auto(&old.picture) && auto(&p) {
        p.placement = old.picture.placement.clone();
    }
    // Likewise the clip's opacity, set with no Opacity entry in the fields
    // to hold it (`anchored`: whether they hold one after the write).
    if !anchored {
        p.opacity = old.picture.opacity;
    }
    // The nodes beside the chain, then every edge that isn't the chain's.
    p.graph.nodes.extend(old.picture.graph.nodes.iter().filter(|n| !old_chain.contains(&n.id)).cloned());
    for (id, position) in &old.picture.graph.ui.positions {
        if p.graph.nodes.iter().any(|n| n.id == *id) {
            p.graph.ui.positions.insert(*id, position.clone());
        }
    }
    let is_chain_edge = |e: &klipp_graph_format::Edge| old_chain.contains(&e.from.0) && old_chain.contains(&e.to.0) && e.from.1.as_str() == "out" && e.to.1.as_str() == "in";
    for e in old.picture.graph.edges.iter().filter(|e| !is_chain_edge(e)) {
        // The lift's own wiring (the source into the grade's frame) is in
        // the lift already: only what only the picture holds is carried.
        if p.graph.edges.iter().any(|f| f.to == e.to) {
            continue;
        }
        for end in [e.from.0, e.to.0] {
            if !p.graph.nodes.iter().any(|n| n.id == end) {
                return Err(format!("it takes away {end}, which the picture's mask wiring reads"));
            }
        }
        p.graph.edges.push(e.clone());
    }
    // The grade's published ports, while what each is bound to is there.
    let interface = old.picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).and_then(|g| g.group.as_ref()).map(|g| g.interface.clone()).unwrap_or_default();
    if !interface.is_empty() {
        let grade = p.graph.nodes.iter_mut().find(|n| n.type_id.as_str() == lt::GROUP).and_then(|g| g.group.as_mut()).ok_or("it takes away the grade, whose ports the picture publishes")?;
        for port in &interface {
            if !grade.graph.nodes.iter().any(|n| n.id == port.binding.node) {
                return Err(format!("it takes away the grade node {} that the grade's port {} publishes", port.binding.node, port.key.as_str()));
            }
        }
        grade.interface = interface;
    }
    p.requires = old.picture.requires.clone();
    p.mark_grade_mix();
    Ok(LiftedPicture { picture: p, versions: fresh.versions })
}

/// Whether a node of the picture's grade holds `id` inside it: a compound
/// carries its inner graph in its entry, a group in its body.
fn grade_nests(p: &ClipPicture, id: NodeId) -> bool {
    let id = id.to_string();
    grade_nodes(p).any(|n| {
        n.group.as_ref().is_some_and(|g| serde_json::to_string(&g.graph).is_ok_and(|t| t.contains(&id)))
            || n.legacy.as_ref().is_some_and(|l| l.payload.to_string().contains(&id))
    })
}

/// The nodes inside the picture's grade (its group's body).
fn grade_nodes(p: &ClipPicture) -> impl Iterator<Item = &Node> {
    p.graph.nodes.iter().filter(|n| n.type_id.as_str() == lt::GROUP).filter_map(|g| g.group.as_ref()).flat_map(|g| g.graph.nodes.iter())
}

fn grade_nodes_mut(p: &mut ClipPicture) -> impl Iterator<Item = &mut Node> {
    p.graph.nodes.iter_mut().filter(|n| n.type_id.as_str() == lt::GROUP).filter_map(|g| g.group.as_mut()).flat_map(|g| g.graph.nodes.iter_mut())
}

#[cfg(test)]
mod keyframe_alias_tests {
    use crate::Clip;
    use klipp_core::{MediaId, RationalTime};
    use klipp_graph_format::path::{NodePath, Root};
    use klipp_graph_format::{ParamKey, ParamValue};
    use klipp_graph_lift::ops::GraphOp;

    /// A clip holding `tag`'s sample, and its node's path.
    fn clip(tag: &str) -> (Clip, NodePath) {
        let t = |s: f64| RationalTime::from_secs_f64(s, 24);
        let c = crate::test_clip::TestClip::of(Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0)))
            .effect((klipp_effects::registry::effect_type(tag).unwrap().sample)())
            .build();
        let node = c.picture_v2().unwrap().picture.graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some(tag)).unwrap().id;
        let id = c.id;
        (c, NodePath::new(Root::Clip { clip: id.0 }, [node]))
    }

    fn key(t: f64, v: f32) -> serde_json::Value {
        serde_json::to_value(klipp_core::Keyframe { time_secs: t, value: v, interpolation: Default::default(), tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() }).unwrap()
    }

    fn times(c: &Clip, path: &NodePath, name: &str) -> Vec<f64> {
        c.node_key_state(path, &ParamKey::from(name)).map(|s| s.keys.iter().map(|k| k.time_secs).collect()).unwrap_or_default()
    }

    /// Every keyframe alias (a ChromaKey's `spill`, stored as `soft_edge`):
    /// a path edit naming either name edits the one stored animation and
    /// value. Before, a key added by the alias went into a second, empty
    /// animation whose write replaced the stored lane, losing every other
    /// key.
    #[test]
    fn an_edit_by_a_keyframe_alias_reaches_the_stored_animation() {
        for (tag, alias, stored) in klipp_graph_lift::names::KEYFRAME_ALIASES {
            let (mut c, path) = clip(tag);
            let add = |name: &str, t: f64, v: f32| GraphOp::AddKeyAt { path: path.clone(), key: name.into(), keyframe: key(t, v) };
            c.apply_picture_ops(&[add(alias, 0.5, 0.2)]).unwrap();
            c.apply_picture_ops(&[add(alias, 1.5, 0.8)]).unwrap();
            assert_eq!(times(&c, &path, stored), vec![0.5, 1.5], "{tag}: two keys added by {alias}, both kept under {stored}");
            assert_eq!(times(&c, &path, alias), vec![0.5, 1.5], "{tag}: and read by either name");
            c.apply_picture_ops(&[add(stored, 1.0, 0.5)]).unwrap();
            assert_eq!(times(&c, &path, stored), vec![0.5, 1.0, 1.5], "{tag}: one by each name, one animation");

            c.apply_picture_ops(&[GraphOp::RemoveKeyAt { path: path.clone(), key: alias.into(), time_secs: 1.0 }]).unwrap();
            assert_eq!(times(&c, &path, stored), vec![0.5, 1.5], "{tag}: a remove by {alias} takes that key only");

            let anim = klipp_graph_format::Animation { keys: vec![key(0.25, 0.1), key(0.75, 0.3), key(1.25, 0.9)], modifiers: None };
            c.apply_picture_ops(&[GraphOp::SetAnim { path: path.clone(), key: alias.into(), anim: Some(anim) }]).unwrap();
            assert_eq!(times(&c, &path, stored), vec![0.25, 0.75, 1.25], "{tag}: an animation set by {alias} is {stored}'s");

            c.apply_picture_ops(&[GraphOp::SetParam { path: path.clone(), key: alias.into(), value: ParamValue::Float(0.375) }]).unwrap();
            let field = serde_json::to_value(&crate::test_clip::effects(&c)[0]).unwrap()[stored].clone();
            assert_eq!(field, serde_json::json!(0.375f32), "{tag}: a value set by {alias} is the stored {stored}");
            let read = |name: &str| c.node_key_state(&path, &ParamKey::from(name)).and_then(|s| s.value);
            assert_eq!((read(alias), read(stored)), (Some(0.375), Some(0.375)), "{tag}: the value read by either name");
        }
    }
}
