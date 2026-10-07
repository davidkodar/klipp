//! Revision 3: a project whose effect node ids are the effects' identity.
//!
//! Revision 2 already writes every effect as a node with an id, but its
//! reader derived the ids again from the effects' slots, so a reorder
//! renamed them. Revision 3 reads the ids as stored. Only the project's
//! `format_revision` changes; side stores (Looks, stills, colour presets)
//! hold no effects and stay at revision 2.
//!
//! What a revision-2 build wrote, and what revision 3 reads it as:
//! - a clip whose fields hold its picture: its effect nodes have their slot
//!   ids (`entry_id(Clip, <clip>, "effects", <slot>)`), the ids revision 3
//!   reads for them;
//! - a picture-truth clip: its picture is its truth and its ids are its
//!   own, kept through every edit, so they need not be slot ids. They are
//!   kept as they are, never renumbered, once proven unique and every edge,
//!   output and group binding names a node that is there.
//!
//! Track effect lists keep slot-derived ids in revision 3 too, so a
//! track's effect nodes must have them.

use std::collections::BTreeSet;

use crate::ids::{entry_id, RootKind};
use crate::old_fields::V2_FIELD;
use crate::revision2::{PROJECT_REVISION, REVISION};
use klipp_graph_format::lifted::ClipPicture;
use klipp_graph_format::{types, Graph, NodeId};
use serde_json::Value;
use uuid::Uuid;

fn list<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default()
}

fn uuid_of(v: &Value, what: &str) -> Result<Uuid, String> {
    v.get("id").and_then(Value::as_str).and_then(|s| Uuid::parse_str(s).ok()).ok_or_else(|| format!("{what} without an id"))
}

/// Every effect node of `nodes` has the slot id of its `kind` and `root`.
fn slot_ids(nodes: &[klipp_graph_format::Node], kind: RootKind, root: Uuid, what: &str) -> Result<(), String> {
    for n in nodes.iter().filter(|n| n.type_id.as_str() != types::LEGACY_OFX && n.origin.is_none()) {
        let Some(slot) = n.legacy.as_ref().and_then(|l| l.slot) else { continue };
        let want = entry_id(kind, root, "effects", slot as usize);
        if n.id != want {
            return Err(format!("{what}: the effect at slot {slot} has id {}, not its slot id {want}", n.id));
        }
    }
    Ok(())
}

/// A truth picture's ids are whole: unique in each graph, and every edge,
/// output and group binding names a node of its graph.
fn ids_whole(graph: &Graph, what: &str) -> Result<(), String> {
    let mut ids: BTreeSet<NodeId> = BTreeSet::new();
    for n in &graph.nodes {
        if !ids.insert(n.id) {
            return Err(format!("{what}: two nodes have id {}", n.id));
        }
    }
    for e in &graph.edges {
        for end in [e.from.0, e.to.0] {
            if !ids.contains(&end) {
                return Err(format!("{what}: an edge names {end}, which isn't a node"));
            }
        }
    }
    if !graph.nodes.is_empty() && !ids.contains(&graph.output) {
        return Err(format!("{what}: the output {} isn't a node", graph.output));
    }
    for n in &graph.nodes {
        if let Some(body) = &n.group {
            let inner = format!("{what} > group {}", n.id);
            ids_whole(&body.graph, &inner)?;
            let inside: BTreeSet<NodeId> = body.graph.nodes.iter().map(|m| m.id).collect();
            if let Some(p) = body.interface.iter().find(|p| !inside.contains(&p.binding.node)) {
                return Err(format!("{inner}: port {} binds {}, which isn't a node", p.key.0, p.binding.node));
            }
        }
    }
    Ok(())
}

fn sequences(project: &Value) -> Vec<&Value> {
    match project.get("sequences") {
        Some(s) => s.as_array().map(|a| a.iter().collect()).unwrap_or_default(),
        None => project.get("sequence").into_iter().collect(),
    }
}

/// What a revision-2 project's check found: its clips by kind.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Checked {
    pub clips: usize,
    pub truth_clips: usize,
    pub tracks_with_effects: usize,
}

/// Check that a revision-2 project reads the same at revision 3 (see the
/// module's note). Refuses a project at any other revision.
pub fn check(raw: &Value) -> Result<Checked, String> {
    match raw.get("format_revision").and_then(Value::as_u64) {
        Some(REVISION) => {}
        Some(r) => return Err(format!("at revision {r}, not {REVISION}")),
        None => return Err(format!("no format revision, not {REVISION}")),
    }
    let mut checked = Checked::default();
    for seq in sequences(raw) {
        for track in list(seq, "tracks") {
            let tid = uuid_of(track, "a track")?;
            if let Some(p) = track.get("effects").and_then(|e| e.get(V2_FIELD)) {
                // A track's picture is its effect list as nodes.
                let nodes: Vec<klipp_graph_format::Node> = serde_json::from_value(p.get("effects").cloned().unwrap_or_default()).map_err(|e| format!("track {tid}: its picture doesn't read: {e}"))?;
                slot_ids(&nodes, RootKind::Track, tid, &format!("track {tid}"))?;
                checked.tracks_with_effects += 1;
            }
            for clip in list(track, "clips") {
                let cid = uuid_of(clip, "a clip")?;
                let Some(p) = clip.get(V2_FIELD) else { continue };
                let what = format!("clip {cid}");
                let picture: ClipPicture = serde_json::from_value(p.clone()).map_err(|e| format!("{what}: its picture doesn't read: {e}"))?;
                checked.clips += 1;
                if picture.is_truth() {
                    ids_whole(&picture.graph, &what)?;
                    checked.truth_clips += 1;
                } else {
                    slot_ids(&picture.graph.nodes, RootKind::Clip, cid, &what)?;
                }
            }
        }
    }
    Ok(checked)
}

/// The revision-3 project from a revision-2 one: the same, with
/// `format_revision: 3`, once [`check`] passes.
pub fn project(raw: &Value) -> Result<Value, String> {
    check(raw)?;
    let mut out = raw.clone();
    out.as_object_mut().ok_or("not a project")?.insert("format_revision".into(), PROJECT_REVISION.into());
    Ok(out)
}

/// The revision-3 text of a revision-2 project's text: the top-level
/// `format_revision` value changed and every other byte kept, so the file
/// differs from its backup by that number only. Refuses unless the text
/// holds exactly one `"format_revision"`.
pub fn project_text(text: &str) -> Result<String, String> {
    let raw: Value = serde_json::from_str(text).map_err(|e| format!("doesn't parse: {e}"))?;
    check(&raw)?;
    let key = "\"format_revision\"";
    let mut at = text.match_indices(key).map(|(i, _)| i);
    let (Some(i), None) = (at.next(), at.next()) else {
        return Err("the text doesn't hold exactly one \"format_revision\"".into());
    };
    let rest = &text[i + key.len()..];
    let colon = rest.find(':').filter(|c| rest[..*c].trim().is_empty()).ok_or("no ':' after \"format_revision\"")?;
    let value = &rest[colon + 1..];
    let start = value.len() - value.trim_start().len();
    let digits = value[start..].bytes().take_while(u8::is_ascii_digit).count();
    if value[start..start + digits] != REVISION.to_string() {
        return Err("\"format_revision\" isn't followed by the revision".into());
    }
    let from = i + key.len() + colon + 1 + start;
    let out = format!("{}{}{}", &text[..from], PROJECT_REVISION, &text[from + digits..]);
    // The edit must have hit the top-level field.
    let back: Value = serde_json::from_str(&out).map_err(|e| format!("the edited text doesn't parse: {e}"))?;
    if back != project(&raw)? {
        return Err("the edited text isn't the project at revision 3".into());
    }
    Ok(out)
}

/// The oracle: `written` is `original` with only the top-level
/// `format_revision` changed, from 2 to 3. Compared as raw JSON, so every
/// field counts, known to today's reader or not.
pub fn verify(original: &str, written: &str) -> Result<(), String> {
    let mut a: Value = serde_json::from_str(original).map_err(|e| format!("the original doesn't parse: {e}"))?;
    let mut b: Value = serde_json::from_str(written).map_err(|e| format!("the written file doesn't parse: {e}"))?;
    let take = |v: &mut Value| v.as_object_mut().and_then(|o| o.remove("format_revision")).and_then(|r| r.as_u64());
    match (take(&mut a), take(&mut b)) {
        (Some(REVISION), Some(PROJECT_REVISION)) => {}
        (from, to) => return Err(format!("the revision went from {from:?} to {to:?}, not {REVISION} to {PROJECT_REVISION}")),
    }
    if a != b {
        return Err(format!("the written project differs from its original: {}", first_diff(&a, &b, "").unwrap_or_default()));
    }
    Ok(())
}

/// Where two values first differ, as a path.
fn first_diff(a: &Value, b: &Value, at: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys().chain(y.keys()) {
                match (x.get(k), y.get(k)) {
                    (Some(p), Some(q)) => {
                        if let Some(d) = first_diff(p, q, &format!("{at}/{k}")) {
                            return Some(d);
                        }
                    }
                    _ => return Some(format!("{at}/{k}: present on one side only")),
                }
            }
            None
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return Some(format!("{at}: {} items against {}", x.len(), y.len()));
            }
            x.iter().zip(y).enumerate().find_map(|(i, (p, q))| first_diff(p, q, &format!("{at}/{i}")))
        }
        _ if a == b => None,
        _ => Some(format!("{at}: {a} against {b}")),
    }
}
