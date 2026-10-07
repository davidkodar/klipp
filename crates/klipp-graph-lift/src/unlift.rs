//! `unlift` (#1089): a revision-2 project, Look, stills index or colour
//! preset back to the v1 form today's reader loads, so the app reads
//! revision 2 at the file boundary and keeps its model (the switch's stage
//! A). Each stored picture goes back through `to_v1(delift(p))`: the
//! picture it was converted from, as today's loader leaves it. Its
//! property, with `revision2` the way back: `revision2(unlift(v)) == v`.

use klipp_graph_format::lifted::ClipPicture;
use klipp_graph_format::{Graph, Node};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::ids::{grade_id, RootKind};
use crate::old_fields::{self, V2_FIELD};
use crate::{delift, to_v1};

pub type Refusal = String;

/// A stored clip picture as the clip's v1 picture fields; a picture-truth
/// clip's as far as they can hold it ([`delift::view_of`]).
pub fn clip(p: &ClipPicture) -> Result<Value, Refusal> {
    let record = if p.is_truth() { delift::clip(&delift::view_of(p))? } else { delift::clip(p)? };
    to_v1::clip(&serde_json::to_value(&record).map_err(|e| e.to_string())?)
}

/// A stored Grade group body standing alone as its v1 grade graph; `group`
/// is the grade node's id in the store (see [`grade_id`]).
pub fn body(v: &Value, group: Uuid) -> Result<Value, Refusal> {
    let b: Graph = serde_json::from_value(v.clone()).map_err(|e| format!("a grade body: {e}"))?;
    to_v1::grade_only(&delift::grade_only(&b, group)?)
}

fn objects(v: Option<&mut Value>) -> impl Iterator<Item = &mut Map<String, Value>> {
    v.and_then(Value::as_array_mut).into_iter().flatten().filter_map(Value::as_object_mut)
}

fn id_of(m: &Map<String, Value>) -> Result<Uuid, Refusal> {
    m.get("id").and_then(Value::as_str).and_then(|s| Uuid::parse_str(s).ok()).ok_or_else(|| "a store without an id".into())
}

/// Every converted clip's `builtin_effects` and every converted track's
/// `effects.video` must be the sentinel alone.
fn refuse_without_sentinel(p: &Value) -> Result<(), Refusal> {
    let sentinel = serde_json::json!([{ "type": old_fields::SENTINEL_TYPE }]);
    let seqs: Vec<&Value> = match p.get("sequences") {
        Some(Value::Array(s)) => s.iter().collect(),
        _ => p.get("sequence").into_iter().collect(),
    };
    for seq in seqs {
        for track in seq.get("tracks").and_then(Value::as_array).into_iter().flatten() {
            let fx = track.get("effects");
            if fx.and_then(|e| e.get(V2_FIELD)).is_some() && fx.and_then(|e| e.get("video")) != Some(&sentinel) {
                return Err(format!("track {}: a converted track whose old effect list isn't the sentinel, a damaged file", track.get("id").and_then(Value::as_str).unwrap_or("?")));
            }
            for clip in track.get("clips").and_then(Value::as_array).into_iter().flatten() {
                if clip.get(V2_FIELD).is_some() && clip.get("builtin_effects") != Some(&sentinel) {
                    return Err(format!("clip {}: a converted clip whose old effect list isn't the sentinel, a damaged file", clip.get("id").and_then(Value::as_str).unwrap_or("?")));
                }
            }
        }
    }
    Ok(())
}

/// A revision-2 project as the v1 project today's reader loads: every
/// stored picture back in the fields it came from, the sentinels and the
/// revision gone.
pub fn project(v2: &Value) -> Result<Value, Refusal> {
    let mut p = v2.clone();
    p.as_object_mut().ok_or("not a project")?.remove("format_revision");
    // A converted clip or track carries exactly the sentinel in its old
    // effect list: anything else there is a damaged or foreign file, refused
    // rather than half trusted.
    refuse_without_sentinel(&p)?;
    // The sentinels checked and gone: one without a picture refuses.
    old_fields::strip_sentinels(&mut p).map_err(|e| format!("{e:?}"))?;
    for seq in old_fields::sequences_mut(&mut p).map_err(|e| format!("{e:?}"))? {
        let sid = id_of(seq)?;
        if let Some(b) = seq.remove(V2_FIELD) {
            seq.insert("timeline_graph".into(), body(&b, grade_id(RootKind::Sequence, sid, "timeline/"))?);
        }
        for group in objects(seq.get_mut("grade_groups")) {
            let gid = id_of(group)?;
            if let Some(Value::Object(mut stages)) = group.remove(V2_FIELD) {
                for k in ["pre", "post"] {
                    if let Some(b) = stages.remove(k) {
                        group.insert(k.into(), body(&b, grade_id(RootKind::GradeGroup, gid, &format!("{k}/")))?);
                    }
                }
            }
        }
        for track in objects(seq.get_mut("tracks")) {
            let tid = id_of(track)?;
            if let Some(effects) = track.get_mut("effects").and_then(Value::as_object_mut) {
                if let Some(Value::Object(mut v2)) = effects.remove(V2_FIELD) {
                    let list: Vec<Node> = serde_json::from_value(v2.remove("effects").unwrap_or(Value::Array(Vec::new()))).map_err(|e| e.to_string())?;
                    let mut nodes = Vec::new();
                    if let Some(g) = v2.remove("grade") {
                        let b: Graph = serde_json::from_value(g).map_err(|e| e.to_string())?;
                        nodes.extend(delift::grade_only(&b, grade_id(RootKind::Track, tid, ""))?.nodes);
                    }
                    nodes.extend(list);
                    if let Some(last) = nodes.last().map(|n| n.id) {
                        let edges = nodes.windows(2).map(|w| klipp_graph_format::Edge { from: (w[0].id, "out".into()), to: (w[1].id, "in".into()) }).collect();
                        let g = Graph { nodes, edges, output: last, ..Graph::empty() };
                        for (k, v) in to_v1::track(&g)?.as_object().into_iter().flatten() {
                            effects.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
            for clip in objects(track.get_mut("clips")) {
                let cid = id_of(clip)?;
                for (n, version) in objects(clip.get_mut("grade_versions")).enumerate() {
                    if let Some(b) = version.remove(V2_FIELD) {
                        version.insert("node_graph".into(), body(&b, grade_id(RootKind::Clip, cid, &format!("version/{n}/")))?);
                    }
                }
                let Some(picture) = clip.remove(V2_FIELD) else { continue };
                let picture: ClipPicture = serde_json::from_value(picture).map_err(|e| e.to_string())?;
                let fields = self::clip(&picture)?;
                // The clip's sound stacks stay; its picture fields come back.
                clip.remove("effects");
                let mut mods: Map<String, Value> = clip.remove("param_modifiers").and_then(|m| m.as_object().cloned()).unwrap_or_default();
                for (k, v) in fields.as_object().into_iter().flatten() {
                    if k == "param_modifiers" {
                        mods.extend(v.as_object().cloned().unwrap_or_default());
                    } else {
                        clip.insert(k.clone(), v.clone());
                    }
                }
                if !mods.is_empty() {
                    clip.insert("param_modifiers".into(), Value::Object(mods));
                }
            }
        }
    }
    Ok(p)
}

/// A revision-2 Look as the v1 Look.
pub fn look(v2: &Value) -> Result<Value, Refusal> {
    let mut o = v2.as_object().cloned().ok_or("a Look isn't an object")?;
    o.remove("format_revision");
    let id = id_of(&o)?;
    if let Some(b) = o.remove(V2_FIELD) {
        o.insert("node_graph".into(), body(&b, grade_id(RootKind::Look, id, ""))?);
    }
    Ok(Value::Object(o))
}

/// A revision-2 stills index as the v1 list.
pub fn stills(v2: &Value) -> Result<Value, Refusal> {
    let list = v2.get("stills").and_then(Value::as_array).ok_or("a revision-2 stills index without its list")?;
    let mut out = Vec::with_capacity(list.len());
    for s in list {
        let mut o = s.as_object().cloned().ok_or("a still isn't an object")?;
        if let Some(b) = o.remove(V2_FIELD) {
            let name = o.get("filename").and_then(Value::as_str).ok_or("a still without its file name")?.to_owned();
            o.insert("grade".into(), body(&b, grade_id(RootKind::Still, Uuid::nil(), &format!("{name}/")))?);
        }
        out.push(Value::Object(o));
    }
    Ok(Value::Array(out))
}

/// A revision-2 colour preset (saved as `<file_name>`) as its v1 graph.
pub fn colour_preset(file_name: &str, v2: &Value) -> Result<Value, Refusal> {
    let b = v2.get(V2_FIELD).ok_or("a revision-2 colour preset without its graph")?;
    body(b, grade_id(RootKind::Preset, Uuid::nil(), &format!("{file_name}/")))
}
