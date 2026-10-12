//! A sequence's own grades keep a correction's mix and mask channel
//! (#1321): the timeline grade and a grade group's pre-clip and post-clip
//! stages, as a clip's picture keeps them (M3 step 17a).
//!
//! In memory such a grade is a v1 graph, which has no place for them, so
//! the app keeps them beside it (as each node's run setting) and the file
//! keeps them in the grade's stored body: a node's `mix` and its
//! `mask.channel`, as a clip's grade body holds them. A body holding one
//! that changes the picture names [`GRADE_MIX_CAPABILITY`] in its
//! `requires`. A stored body is a [`Graph`], which takes no field it
//! doesn't know, so a reader from before refuses the file rather than draw
//! the grade without them. A body with neither is written and read as it
//! was before.

use klipp_graph_format::lifted::{body_grade_mix_used, unknown_capability, Stage, GRADE_MIX_CAPABILITY};
use klipp_graph_format::path::Root;
use klipp_graph_format::{Graph, Node, NodeId, ParamKey, ParamValue};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::old_fields::{self, V2_FIELD};

/// The capabilities a sequence grade's stored body may require.
const KNOWN: &[&str] = &[GRADE_MIX_CAPABILITY];

const REQUIRES: &str = "requires";

/// One correction's mix and mask channel, by its node's id: `None` is
/// whole, and red.
#[derive(Clone, Debug, PartialEq)]
pub struct Blend {
    pub node: NodeId,
    pub mix: Option<f32>,
    pub channel: Option<String>,
}

fn channel_key() -> ParamKey {
    ParamKey::from("mask.channel")
}

/// Every node of `g`, at any depth.
fn nodes(g: &Graph) -> Vec<&Node> {
    let mut out = Vec::new();
    for n in &g.nodes {
        out.push(n);
        if let Some(group) = &n.group {
            out.extend(nodes(&group.graph));
        }
    }
    out
}

fn node_mut(g: &mut Graph, id: NodeId) -> Option<&mut Node> {
    for n in &mut g.nodes {
        if n.id == id {
            return Some(n);
        }
        if let Some(found) = n.group.as_mut().and_then(|group| node_mut(&mut group.graph, id)) {
            return Some(found);
        }
    }
    None
}

/// Each node of `g` holding a mix or a mask channel, in the graph's order.
pub fn blends(g: &Graph) -> Vec<Blend> {
    nodes(g)
        .into_iter()
        .filter_map(|n| {
            let channel = match n.params.get(&channel_key()) {
                Some(ParamValue::Text(c)) => Some(c.clone()),
                _ => None,
            };
            (n.mix.is_some() || channel.is_some()).then_some(Blend { node: n.id, mix: n.mix, channel })
        })
        .collect()
}

/// Set each of `blends` on its node of `g`, any setting already there
/// replaced. A blend whose node `g` doesn't hold is refused, naming it,
/// rather than dropped.
pub fn set_blends(g: &mut Graph, blends: &[Blend]) -> Result<(), String> {
    for b in blends {
        let n = node_mut(g, b.node).ok_or_else(|| format!("node {} holds a mix or mask channel but isn't in the grade", b.node))?;
        n.mix = b.mix;
        match &b.channel {
            Some(c) => {
                n.params.insert(channel_key(), ParamValue::Text(c.clone()));
            }
            None => {
                n.params.remove(&channel_key());
            }
        }
    }
    Ok(())
}

/// A stored body as it is written: `blends` set on its nodes, and its
/// `requires` naming [`GRADE_MIX_CAPABILITY`] when one changes the
/// picture. With no blends it is left exactly as it was.
pub fn write(body: &mut Value, blends: &[Blend]) -> Result<(), String> {
    if blends.is_empty() {
        return Ok(());
    }
    let mut g: Graph = serde_json::from_value(body.clone()).map_err(|e| format!("a grade body: {e}"))?;
    set_blends(&mut g, blends)?;
    g.check_lifted_body().map_err(|e| e.to_string())?;
    *body = serde_json::to_value(&g).map_err(|e| e.to_string())?;
    if body_grade_mix_used(&g) {
        body.as_object_mut().ok_or("a grade body isn't an object")?.insert(REQUIRES.into(), Value::from(vec![GRADE_MIX_CAPABILITY]));
    }
    Ok(())
}

/// A stored body as it is read: its `requires` taken off, so the rest
/// reads as a [`Graph`], and its blends. A capability this reader doesn't
/// know is refused, and so is a body whose `requires` doesn't say what its
/// nodes hold, a damaged file.
pub fn read(body: &mut Value) -> Result<Vec<Blend>, String> {
    let requires: Vec<String> = match body.as_object_mut().ok_or("a grade body isn't an object")?.remove(REQUIRES) {
        Some(r) => serde_json::from_value(r).map_err(|e| format!("a grade body's requires: {e}"))?,
        None => Vec::new(),
    };
    if let Some(unknown) = unknown_capability(&requires, KNOWN) {
        return Err(format!("a sequence grade requires {unknown}, which this build doesn't read"));
    }
    let g: Graph = serde_json::from_value(body.clone()).map_err(|e| format!("a grade body: {e}"))?;
    let said = requires.iter().any(|r| r == GRADE_MIX_CAPABILITY);
    if said != body_grade_mix_used(&g) {
        return Err(if said {
            "a sequence grade requires a correction's mix but holds none, a damaged file".into()
        } else {
            "a sequence grade holds a correction's mix without requiring it, a damaged file".into()
        });
    }
    Ok(blends(&g))
}

fn objects(v: Option<&mut Value>) -> impl Iterator<Item = &mut Map<String, Value>> {
    v.and_then(Value::as_array_mut).into_iter().flatten().filter_map(Value::as_object_mut)
}

fn id_of(m: &Map<String, Value>) -> Result<Uuid, String> {
    m.get("id").and_then(Value::as_str).and_then(|s| Uuid::parse_str(s).ok()).ok_or_else(|| "a store without an id".into())
}

const STAGES: [(&str, Stage); 2] = [("pre", Stage::Pre), ("post", Stage::Post)];

/// Each stored sequence grade body of a project at revision 2 or later,
/// by its sequence's id and its root, for `f`. By both, because a
/// duplicated sequence keeps its grade groups' ids.
fn for_each_body(project: &mut Value, mut f: impl FnMut(Uuid, Root, &mut Value) -> Result<(), String>) -> Result<(), String> {
    for seq in old_fields::sequences_mut(project).map_err(|e| format!("{e:?}"))? {
        let sid = id_of(seq)?;
        if let Some(b) = seq.get_mut(V2_FIELD) {
            f(sid, Root::TimelineGrade { sequence: sid }, b)?;
        }
        for group in objects(seq.get_mut("grade_groups")) {
            let gid = id_of(group)?;
            if let Some(stages) = group.get_mut(V2_FIELD).and_then(Value::as_object_mut) {
                for (k, stage) in STAGES {
                    if let Some(b) = stages.get_mut(k) {
                        f(sid, Root::GroupStage { group: gid, stage }, b)?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// A stored body's blends taken out of it (#1322): [`read()`]'s checks,
/// then each node's mix and mask channel cleared, so what is left is the
/// body a grade with none would store.
pub fn take(body: &mut Value) -> Result<Vec<Blend>, String> {
    let blends = read(body)?;
    if blends.is_empty() {
        return Ok(blends);
    }
    let mut g: Graph = serde_json::from_value(body.clone()).map_err(|e| format!("a grade body: {e}"))?;
    let cleared: Vec<Blend> = blends.iter().map(|b| Blend { node: b.node, mix: None, channel: None }).collect();
    set_blends(&mut g, &cleared)?;
    *body = serde_json::to_value(&g).map_err(|e| e.to_string())?;
    Ok(blends)
}

/// Each clip grade version's stored body in a project at revision 2 or
/// later, by its clip's id and its place among the clip's versions, for
/// `f` (#1322).
fn for_each_version(project: &mut Value, mut f: impl FnMut(Uuid, usize, &mut Value) -> Result<(), String>) -> Result<(), String> {
    for seq in old_fields::sequences_mut(project).map_err(|e| format!("{e:?}"))? {
        for track in objects(seq.get_mut("tracks")) {
            for clip in objects(track.get_mut("clips")) {
                let cid = id_of(clip)?;
                for (n, version) in objects(clip.get_mut("grade_versions")).enumerate() {
                    if let Some(b) = version.get_mut(V2_FIELD) {
                        f(cid, n, b)?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Write each clip grade version's blends, `of` its clip's id and its
/// place, into a project just converted (#1322): the version's v1 graph
/// leaves them out, as a sequence grade's does. Each body is set to
/// exactly those, whatever it held.
pub fn write_versions(project: &mut Value, of: &dyn Fn(Uuid, usize) -> Vec<Blend>) -> Result<(), String> {
    for_each_version(project, |cid, n, b| {
        take(b).and_then(|_| write(b, &of(cid, n))).map_err(|e| format!("clip {cid}'s grade version {n}: {e}"))
    })
}

/// Each clip grade version's blends in a stored project, by its clip's id
/// and its place, its own [`read()`] checks passed. Read only.
pub fn read_versions(project: &Value) -> Result<Vec<(Uuid, usize, Vec<Blend>)>, String> {
    let mut copy = project.clone();
    let mut out = Vec::new();
    for_each_version(&mut copy, |cid, n, b| {
        let blends = read(b).map_err(|e| format!("clip {cid}'s grade version {n}: {e}"))?;
        if !blends.is_empty() {
            out.push((cid, n, blends));
        }
        Ok(())
    })?;
    Ok(out)
}

/// Write each sequence grade's blends, `of` its sequence's id and its
/// root, into a project just converted: every body [`write()`] takes.
pub fn write_project(project: &mut Value, of: &dyn Fn(Uuid, Root) -> Vec<Blend>) -> Result<(), String> {
    for_each_body(project, |sid, root, b| write(b, &of(sid, root)).map_err(|e| format!("{root:?}: {e}")))
}

/// Each sequence grade's blends in a stored project, by its sequence's id
/// and its root, its own [`read()`] checks passed. Read only.
pub fn read_project(project: &Value) -> Result<Vec<(Uuid, Root, Vec<Blend>)>, String> {
    let mut copy = project.clone();
    let mut out = Vec::new();
    for_each_body(&mut copy, |sid, root, b| {
        let blends = read(b).map_err(|e| format!("{root:?}: {e}"))?;
        if !blends.is_empty() {
            out.push((sid, root, blends));
        }
        Ok(())
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lift::lift_grade_only;

    /// A default timeline grade's stored body, and its Corrector's id,
    /// which the lift keeps.
    fn body() -> (Value, NodeId) {
        let grade = klipp_nodes::NodeGraph::default();
        let corrector = grade.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).unwrap().id;
        let raw = serde_json::json!({ "id": Uuid::nil(), "timeline_graph": grade });
        let structural = crate::to_v2::timeline_to_v2(raw.as_object().unwrap()).unwrap().unwrap();
        let b = lift_grade_only(&structural, false).unwrap();
        assert!(b.nodes.iter().any(|n| n.id == corrector), "the lift keeps the Corrector's id");
        (serde_json::to_value(b).unwrap(), corrector)
    }

    /// A body with no blends is written and read exactly as before: no
    /// `requires`, nothing changed.
    #[test]
    fn a_body_without_a_mix_is_as_before() {
        let (b, _) = body();
        let mut written = b.clone();
        write(&mut written, &[]).unwrap();
        assert_eq!(written, b);
        let mut read_back = b.clone();
        assert!(read(&mut read_back).unwrap().is_empty());
        assert_eq!(read_back, b);
    }

    /// A mix and a channel are written on the node, with `requires`, and
    /// read back; a reader that knows no `requires` (a [`Graph`] alone)
    /// refuses the body.
    #[test]
    fn a_mix_and_channel_round_trip_and_an_old_reader_refuses_them() {
        let (b, node) = body();
        let blend = Blend { node, mix: Some(0.4), channel: Some("alpha".into()) };
        let mut written = b.clone();
        write(&mut written, std::slice::from_ref(&blend)).unwrap();
        assert_eq!(written[REQUIRES], serde_json::json!([GRADE_MIX_CAPABILITY]));
        assert!(serde_json::from_value::<Graph>(written.clone()).is_err(), "a reader from before refuses it");
        let mut back = written.clone();
        assert_eq!(read(&mut back).unwrap(), vec![blend]);
        assert!(back.get(REQUIRES).is_none());
    }

    /// A `requires` that doesn't say what the nodes hold, or names a
    /// capability this reader doesn't know, is refused; a blend for a node
    /// the body doesn't hold refuses the write.
    #[test]
    fn damaged_bodies_are_refused() {
        let (b, node) = body();
        let mut written = b.clone();
        write(&mut written, &[Blend { node, mix: Some(0.4), channel: None }]).unwrap();
        let mut dropped = written.clone();
        dropped.as_object_mut().unwrap().remove(REQUIRES);
        assert!(read(&mut dropped).unwrap_err().contains("without requiring"));
        let mut extra = b.clone();
        extra.as_object_mut().unwrap().insert(REQUIRES.into(), serde_json::json!([GRADE_MIX_CAPABILITY]));
        assert!(read(&mut extra).unwrap_err().contains("holds none"));
        let mut unknown = b.clone();
        unknown.as_object_mut().unwrap().insert(REQUIRES.into(), serde_json::json!(["later-1"]));
        assert!(read(&mut unknown).unwrap_err().contains("later-1"));
        let mut missing = b.clone();
        assert!(write(&mut missing, &[Blend { node: Uuid::new_v4(), mix: Some(0.5), channel: None }]).is_err());
    }
}
