//! Edits of a clip's placed kinds (M4 step 12b-0): a Transform, an
//! Opacity, a Speed Ramp or a Temporal Noise Reduction added, removed,
//! moved, switched or replaced through the clip's picture.
//!
//! The lift gives those entries roles rather than keeping them as list
//! nodes: the last enabled Opacity is the clip's opacity (the rest
//! multiplying, #1386), the first enabled Transform is its placement (the
//! rest kept off), and a blending Speed Ramp and the denoiser run at the
//! chain's head. Those rules live in the lift alone ([`crate::lift`]).
//! An edit here is made on the picture's own record (`delift::clip`, a
//! graph-format [`ClipRecordV2`]: each entry with its stable id, its slot,
//! its stored entry and its keys), and the record is lifted again, once a
//! batch: the roles come out exactly as the lift gives them to the edited
//! list ([`settle`]).
//!
//! A picture-truth clip is refused for now: its record is its picture's
//! view, without what only the picture keeps (an effect's mix and mask),
//! which a lift of the record would drop. Splicing those back is
//! follow-up work, before step 12f.

use klipp_graph_format::lifted::ClipPicture;
use klipp_graph_format::{types, ClipRecordV2, Edge, Legacy, Node, NodeId};
use serde_json::{Map, Value};

use crate::lift::{lift_clip, LiftCtx};

/// One edit of a clip's built-in list, by the stable ids its entries hold.
#[derive(Clone, Debug, PartialEq)]
pub enum PlacedEdit {
    /// `entry` (an entry as stored, with its `type`) put in the list after
    /// `after`, or first, under `id`.
    Add { id: NodeId, entry: Value, after: Option<NodeId> },
    Remove(NodeId),
    /// The entry switched on or off, as the person set it: a role it plays
    /// (or stops playing) follows from that.
    SetEnabled(NodeId, bool),
    /// The entry moved to after `after`, or first.
    Move { node: NodeId, after: Option<NodeId> },
    /// The entry replaced whole by another of its kind; its id, flag and
    /// keys stay.
    SetPayload(NodeId, Value),
}

/// Whether `entry` (as stored, with its `type`) is one of the kinds the
/// clip places itself: a node of its own type, not a plain effect node.
pub fn is_placed(entry: &Value) -> bool {
    let tag = entry.get("type").and_then(Value::as_str).unwrap_or_default();
    crate::to_v2::effect_node_type(tag) != types::LEGACY_EFFECT
}

fn is_listed(n: &Node) -> bool {
    !matches!(n.type_id.as_str(), types::LEGACY_GRADE | types::LEGACY_OFX)
}

fn slot(n: &Node) -> usize {
    n.legacy.as_ref().and_then(|l| l.slot).unwrap_or(usize::MAX)
}

/// The record's built-in entries' ids, in list order.
fn list_order(record: &ClipRecordV2) -> Vec<NodeId> {
    let mut listed: Vec<&Node> = record.graph.nodes.iter().filter(|n| is_listed(n)).collect();
    listed.sort_by_key(|n| slot(n));
    listed.into_iter().map(|n| n.id).collect()
}

/// `key` with its slot `from` renamed `to`: `"{slot}:{param}"` for a key's
/// name, `"effect:{slot}:{param}"` for a modifier's.
fn reslot(key: &str, from: usize, to: usize) -> String {
    if let Some(param) = key.strip_prefix(&format!("{from}:")) {
        return format!("{to}:{param}");
    }
    if let Some(param) = key.strip_prefix(&format!("effect:{from}:")) {
        return format!("effect:{to}:{param}");
    }
    key.to_owned()
}

/// Each entry at its place in `order`, its keys and modifiers moving with
/// its slot; the record's chain as `delift::clip` builds it (the grade,
/// the plug-ins, then the list by slot).
fn renumber(record: &mut ClipRecordV2, order: &[NodeId]) -> Result<(), String> {
    for (to, id) in order.iter().enumerate() {
        let n = record.graph.nodes.iter_mut().find(|n| n.id == *id).ok_or_else(|| format!("entry {id} isn't in the clip's list"))?;
        let legacy = n.legacy.as_mut().ok_or_else(|| format!("entry {id} carries no stored entry"))?;
        let from = legacy.slot.unwrap_or(to);
        if from != to {
            if let Some(Value::Object(keys)) = legacy.keyframes.take() {
                legacy.keyframes = Some(Value::Object(keys.into_iter().map(|(k, v)| (reslot(&k, from, to), v)).collect::<Map<String, Value>>()));
            }
            legacy.modifiers = std::mem::take(&mut legacy.modifiers).into_iter().map(|(k, v)| (reslot(&k, from, to), v)).collect();
        }
        legacy.slot = Some(to);
    }
    let mut nodes = std::mem::take(&mut record.graph.nodes);
    let (mut listed, rest): (Vec<Node>, Vec<Node>) = nodes.drain(..).partition(is_listed);
    listed.sort_by_key(slot);
    let (grade, ofx): (Vec<Node>, Vec<Node>) = rest.into_iter().partition(|n| n.type_id.as_str() == types::LEGACY_GRADE);
    let mut ofx = ofx;
    ofx.sort_by_key(slot);
    let chain: Vec<Node> = grade.into_iter().chain(ofx).chain(listed).collect();
    record.graph.edges = chain.windows(2).map(|w| Edge { from: (w[0].id, "out".into()), to: (w[1].id, "in".into()) }).collect();
    if let Some(last) = chain.last() {
        record.graph.output = last.id;
    }
    record.graph.nodes = chain;
    Ok(())
}

/// The place in `order` after `after` (0 for first); `after` must be an
/// entry of the list.
fn place_after(order: &[NodeId], after: Option<NodeId>) -> Result<usize, String> {
    match after {
        None => Ok(0),
        Some(a) => order.iter().position(|id| *id == a).map(|i| i + 1).ok_or_else(|| format!("entry {a} isn't in the clip's list")),
    }
}

fn entry_type(entry: &Value) -> Result<&'static str, String> {
    let tag = entry.get("type").and_then(Value::as_str).ok_or("an entry without its type")?;
    Ok(crate::to_v2::effect_node_type(tag))
}

/// `edits`, in turn, on `record`'s list.
fn apply(record: &mut ClipRecordV2, edits: &[PlacedEdit]) -> Result<(), String> {
    let mut order = list_order(record);
    let node = |record: &mut ClipRecordV2, id: NodeId| -> Result<&mut Node, String> {
        record.graph.nodes.iter_mut().filter(|n| is_listed(n)).find(|n| n.id == id).ok_or_else(|| format!("entry {id} isn't in the clip's list"))
    };
    for edit in edits {
        match edit {
            PlacedEdit::Add { id, entry, after } => {
                if order.contains(id) || record.graph.nodes.iter().any(|n| n.id == *id) {
                    return Err(format!("entry {id} is in the clip already"));
                }
                let at = place_after(&order, *after)?;
                record.graph.nodes.push(Node {
                    id: *id,
                    type_id: entry_type(entry)?.into(),
                    type_version: 1,
                    label: String::new(),
                    enabled: true,
                    params: Default::default(),
                    anim: Default::default(),
                    mix: None,
                    wireless: Default::default(),
                    group: None,
                    legacy: Some(Legacy { slot: Some(usize::MAX), payload: entry.clone(), keyframes: None, modifiers: Default::default() }),
                    legacy_flags: None,
                    origin: None,
                });
                order.insert(at, *id);
            }
            PlacedEdit::Remove(id) => {
                node(record, *id)?;
                record.graph.nodes.retain(|n| n.id != *id);
                order.retain(|o| o != id);
            }
            PlacedEdit::SetEnabled(id, on) => {
                let n = node(record, *id)?;
                n.enabled = *on;
            }
            PlacedEdit::Move { node: id, after } => {
                node(record, *id)?;
                if *after == Some(*id) {
                    return Err(format!("entry {id} can't go after itself"));
                }
                order.retain(|o| o != id);
                let at = place_after(&order, *after)?;
                order.insert(at, *id);
            }
            PlacedEdit::SetPayload(id, payload) => {
                let kind = entry_type(payload)?;
                let n = node(record, *id)?;
                if n.type_id.as_str() != kind {
                    return Err(format!("entry {id} is a {}, not a {kind}", n.type_id.as_str()));
                }
                n.legacy.as_mut().ok_or_else(|| format!("entry {id} carries no stored entry"))?.payload = payload.clone();
            }
        }
    }
    renumber(record, &order)
}

/// `picture` with `edits` made on its list, the roles they leave settled by
/// the lift, once ([`settle`]); and the record it was lifted from, which a
/// clip's fields are written from. Refused, naming why, on a picture-truth
/// clip, and where the lift would lose part of the clip.
pub fn edit(picture: &ClipPicture, ctx: &LiftCtx, edits: &[PlacedEdit]) -> Result<(ClipRecordV2, ClipPicture), String> {
    if picture.is_truth() {
        return Err("a clip whose picture holds what its list can't (an effect's mix or mask) can't take an edit of a Transform, an Opacity, a Speed Ramp or a Temporal Noise Reduction yet".into());
    }
    let mut record = crate::delift::clip(picture)?;
    apply(&mut record, edits)?;
    let settled = settle_record(&record, ctx)?;
    Ok((record, settled))
}

/// `picture`'s roles as the lift gives them to its list: its own record
/// lifted again. An ordinary clip's settled picture is itself.
pub fn settle(picture: &ClipPicture, ctx: &LiftCtx) -> Result<ClipPicture, String> {
    settle_record(&crate::delift::clip(picture)?, ctx)
}

fn settle_record(record: &ClipRecordV2, ctx: &LiftCtx) -> Result<ClipPicture, String> {
    let (picture, notes) = lift_clip(record, ctx)?;
    if let Some(lost) = notes.iter().find(|n| crate::revision2::is_lost(n)) {
        return Err(format!("the edit would lose part of the clip ({lost:?})"));
    }
    picture.graph.check_lifted(false).map_err(|e| e.to_string())?;
    Ok(picture)
}
