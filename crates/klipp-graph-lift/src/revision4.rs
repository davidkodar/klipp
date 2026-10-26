//! Picture revision 4 (M4 step F): one conversion of every project
//! for picture revision 4's changes (#1391, #1406, #1409, #1386, #1394,
//! #1321, the frei0r and OFX plug-in nodes), each made by its own row's
//! change before this step converts with them (`Klipp-notes` design, M4
//! plan v2 §4).
//!
//! [`convert`] takes a project file's text at revision 3 and gives its
//! revision-4 text, with the declared changes it made (the rows that
//! change what a project draws). It writes nothing of its own: the project
//! is read back to the v1 form it was lifted from ([`unlift`]), and
//! written again by the app's own writer under today's lift
//! ([`revision2::project_with`]), so each row's change is the lift's.
//! Before that, the one row the lift doesn't make is made in the v1 form:
//! a Speed Ramp's fields that never drew are dropped, and a frozen one,
//! which never blended, becomes speed 1 without blending (#1409).
//!
//! A picture-truth clip's picture is the authority, not its v1 view, so
//! it goes to the writer whole ([`truth_picture`]): its plug-ins take
//! their revision-4 nodes as it is stored, a Fixed placement whose height
//! differs from its width gains its aspect (#1406, declared), and a
//! Speed Ramp's dropped fields go. A truth picture holding anything else
//! a row changes (a compound, a frozen ramp, two Opacities) is refused,
//! named: the corpus holds none, and a guess would be worse.
//!
//! Each effect keeps the node id revision 3 stored for it, by its place
//! in its list (`keep_ids`): the v1 form holds none.
//!
//! **What it proves, and what it doesn't.** Before any revision-4 rule, a
//! non-truth clip's revision-3 picture is checked to be read whole by
//! [`unlift`]: its record (`delift`) is the one the v1 form gives back
//! (`to_v2`), so anything unlift would drop stops the file, named, rather
//! than show later as a changed frame. After the conversion, the project
//! unlifted and written again is the same document (`revision2(unlift(p))
//! == p`): a fixed point of the writer, not a proof that the project draws
//! what it drew. A picture-truth clip passes through whole on both sides.
//! What a project draws is held by the dry run's render check (every path,
//! before and after) and by the declared changes.
//!
//! **Declared changes.** #1406 (a picture that draws an aspect: no revision-
//! 3 render drew one), #1386 (an Opacity that now multiplies, or an
//! adjustment layer's opacity by the clip's rule), #1409 (a frozen Speed
//! Ramp), #1391 (a compound kept off, the same picture). #1321 and #1394
//! change no drawing and declare nothing: a revision-3 file can't hold a
//! sequence grade's mix, or an automatic placement that moves (the Format
//! family's cases hold both to "unchanged but the revision"). The plug-in
//! nodes change no drawing either.

use std::collections::BTreeMap;

use klipp_graph_format::lifted::{types as lt, ClipPicture, LiftNote};
use klipp_graph_format::{types, Graph, Node};
use serde_json::{Map, Value};

use crate::old_fields::V2_FIELD;
use crate::{canon, revision2, unlift};

/// A project's revision-4 text, and what it declares.
#[derive(Debug, Clone, PartialEq)]
pub struct Converted {
    pub text: String,
    pub declared: Vec<Declared>,
}

/// A declared change: the row (its issue) and what of the project it
/// changes, for the conversion's record and the readiness report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    pub row: &'static str,
    pub what: String,
}

/// The revision this step reads.
pub const FROM: u64 = 3;
/// The revision it converts to (revision 5 came after, #2754).
pub const TO: u64 = 4;

/// The fields of a Speed Ramp entry revision 4 drops (#1409): none of
/// them drew.
pub const DROPPED_RAMP_FIELDS: [&str; 6] = ["ramp_curve", "ramp_start", "ramp_end", "reverse", "freeze_at", "freeze"];

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn objects(v: Option<&mut Value>) -> impl Iterator<Item = &mut Map<String, Value>> {
    v.and_then(Value::as_array_mut).into_iter().flatten().filter_map(Value::as_object_mut)
}

fn id_of(m: &Map<String, Value>) -> String {
    m.get("id").and_then(Value::as_str).unwrap_or("?").to_owned()
}

/// `project`'s text at revision 4, or why it isn't converted. `catalog`
/// describes the node types, so a stored picture is checked as the app
/// checks it on load (an effect's mask included).
pub fn convert(project: &str, catalog: &dyn klipp_graph_format::PortCatalog) -> Result<Converted, String> {
    let v3: Value = serde_json::from_str(project).map_err(|e| format!("the project doesn't read: {e}"))?;
    match v3.get("format_revision").and_then(Value::as_u64) {
        Some(FROM) => {}
        other => return Err(format!("a project at revision {other:?}, not {FROM}")),
    }
    let mut declared = Vec::new();

    // The truth clips' pictures, by sequence and clip, as the writer takes
    // them (in memory, with their grade versions' bodies).
    let mut truth: BTreeMap<(String, String), (ClipPicture, Vec<Value>)> = BTreeMap::new();
    let mut before: BTreeMap<(String, String), ClipPicture> = BTreeMap::new();
    for seq in v3.get("sequences").and_then(Value::as_array).into_iter().flatten() {
        let seq_id = seq.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
        for track in seq.get("tracks").and_then(Value::as_array).into_iter().flatten() {
            for clip in track.get("clips").and_then(Value::as_array).into_iter().flatten() {
                let (Some(id), Some(stored)) = (clip.get("id").and_then(Value::as_str), clip.get(V2_FIELD)) else { continue };
                let picture: ClipPicture = serde_json::from_value(stored.clone()).map_err(|e| format!("clip {id}: its picture doesn't read: {e}"))?;
                // A damaged picture (two nodes of one id, an edge to none)
                // stops the file, named, as it stops the app's loader.
                picture.graph.check_lifted_with_catalog(false, catalog).map_err(|e| format!("clip {id}: its picture isn't one the lift makes: {e}"))?;
                if !picture.is_truth() {
                    read_whole(id, &picture)?;
                }
                if picture.is_truth() {
                    let versions: Vec<Value> = clip.get("grade_versions").and_then(Value::as_array).into_iter().flatten().filter_map(|v| v.get(V2_FIELD).cloned()).collect();
                    for b in &versions {
                        if has_compound_body(b) {
                            return Err(format!("clip {id}: a grade version holds a compound, which a picture-truth clip can't convert (#1391)"));
                        }
                    }
                    let p = truth_picture(id, picture.clone(), &mut declared)?;
                    truth.insert((seq_id.clone(), id.to_owned()), (p, versions));
                }
                before.insert((seq_id.clone(), id.to_owned()), picture);
            }
        }
    }

    // The v1 form, its Speed Ramps as revision 4 keeps them.
    let mut v1 = unlift::project(&v3)?;
    ramps(&mut v1, &mut declared);

    let stored = |seq: &str, clip: &str| truth.get(&(seq.to_owned(), clip.to_owned())).cloned();
    let (mut v4, notes) = revision2::project_with(&v1, &stored)?;
    v4.as_object_mut().ok_or("not a project")?.insert("format_revision".into(), Value::from(TO));
    // Each effect keeps the node id revision 3 stored for it (M3 proposal
    // 6): the v1 form has none, so the lift gave each its slot's again.
    keep_ids(&mut v4, &before)?;
    declare_lifted(&v4, &before, &notes, &mut declared)?;

    // The proof: unlifted and written again, the same document.
    let again = revision2::project_with(&unlift::project(&v4)?, &stored)?.0;
    let mut again = again;
    again.as_object_mut().ok_or("not a project")?.insert("format_revision".into(), Value::from(TO));
    keep_ids(&mut again, &pictures(&v4)?)?;
    if canon::sorted(again) != canon::sorted(v4.clone()) {
        return Err("the revision-4 project, unlifted and written again, is another document".into());
    }
    let text = serde_json::to_string_pretty(&v4).map_err(err)?;
    Ok(Converted { text, declared })
}

/// Every clip's stored picture in `project`, by sequence and clip.
fn pictures(project: &Value) -> Result<BTreeMap<(String, String), ClipPicture>, String> {
    let mut out = BTreeMap::new();
    for seq in project.get("sequences").and_then(Value::as_array).into_iter().flatten() {
        let seq_id = seq.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
        for track in seq.get("tracks").and_then(Value::as_array).into_iter().flatten() {
            for clip in track.get("clips").and_then(Value::as_array).into_iter().flatten() {
                if let (Some(id), Some(p)) = (clip.get("id").and_then(Value::as_str), clip.get(V2_FIELD)) {
                    out.insert((seq_id.clone(), id.to_owned()), serde_json::from_value(p.clone()).map_err(|e| format!("clip {id}: {e}"))?);
                }
            }
        }
    }
    Ok(out)
}

/// An entry's place in its list: a plug-in's or a built-in's slot.
fn place(n: &Node) -> Option<(bool, u32)> {
    Some((n.type_id.as_str() == types::LEGACY_OFX || n.type_id.as_str() == lt::OFX, n.legacy.as_ref()?.slot?))
}

/// `graph`'s entries renamed to the ids `like` has at the same places, in
/// its nodes, its edges and its output; whether any was.
fn rename_by_place(graph: &mut Graph, like: &Graph) -> bool {
    let was: BTreeMap<(bool, u32), klipp_graph_format::NodeId> = like.nodes.iter().filter_map(|n| Some((place(n)?, n.id))).collect();
    let rename: BTreeMap<klipp_graph_format::NodeId, klipp_graph_format::NodeId> =
        graph.nodes.iter().filter_map(|n| { let to = *was.get(&place(n)?)?; (to != n.id).then_some((n.id, to)) }).collect();
    if rename.is_empty() {
        return false;
    }
    let r = |x: klipp_graph_format::NodeId| rename.get(&x).copied().unwrap_or(x);
    for n in &mut graph.nodes {
        n.id = r(n.id);
    }
    for e in &mut graph.edges {
        e.from.0 = r(e.from.0);
        e.to.0 = r(e.to.0);
    }
    graph.output = r(graph.output);
    true
}

/// Each clip's built-in and plug-in entries in `project` renamed to the ids
/// `ids` stored for the entry at the same list place (an effect's slot, a
/// plug-in's), wherever the lift gave another: in its nodes, its edges and
/// its output. A picture-truth clip went through whole and needs none.
fn keep_ids(project: &mut Value, ids: &BTreeMap<(String, String), ClipPicture>) -> Result<(), String> {
    for seq in objects(project.get_mut("sequences")) {
        let seq_id = seq.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
        for track in objects(seq.get_mut("tracks")) {
            for clip in objects(track.get_mut("clips")) {
                let id = id_of(clip);
                let Some(old) = ids.get(&(seq_id.clone(), id.clone())) else { continue };
                let Some(stored) = clip.get_mut(V2_FIELD) else { continue };
                let mut p: ClipPicture = serde_json::from_value(stored.clone()).map_err(|e| format!("clip {id}: {e}"))?;
                if p.is_truth() {
                    continue;
                }
                if !rename_by_place(&mut p.graph, &old.graph) {
                    continue;
                }
                *stored = serde_json::to_value(&p).map_err(err)?;
            }
        }
    }
    Ok(())
}

/// Whether [`unlift`] reads a non-truth revision-3 picture whole: the
/// record it was lifted from (`delift`) is the one the v1 form unlift gives
/// back reads as (`to_v2`). One that isn't would lose part of the picture
/// in the conversion, so the file stops, naming the clip.
fn read_whole(clip: &str, picture: &ClipPicture) -> Result<(), String> {
    let record = crate::delift::clip(picture).map_err(|e| format!("clip {clip}: its picture doesn't read back: {e}"))?;
    let v1 = unlift::clip(picture).map_err(|e| format!("clip {clip}: unlift refuses its picture: {e}"))?;
    let mut with_id = v1.as_object().cloned().unwrap_or_default();
    with_id.insert("id".into(), Value::from(clip));
    let mut again = crate::to_v2::clip_to_v2(&with_id).map_err(|e| format!("clip {clip}: its v1 form doesn't read: {e:?}"))?;
    // The v1 form keeps no effect ids: the stored ones, by place, as the
    // conversion keeps them.
    rename_by_place(&mut again.graph, &record.graph);
    let (mut a, mut b) = (serde_json::to_value(&record).map_err(err)?, serde_json::to_value(&again).map_err(err)?);
    // A compound's subgraph is a v1 grade graph inside a payload: two
    // spellings of one graph (an empty link list or none) are the same.
    subgraphs_canon(&mut a);
    subgraphs_canon(&mut b);
    let (a, b) = (canon::sorted(a), canon::sorted(b));
    if a != b {
        return Err(format!("clip {clip}: unlift loses part of its picture, which the conversion would drop ({})", first_change(&a, &b, "")));
    }
    Ok(())
}

/// Every compound subgraph in `v` normalised as `canon` normalises a v1
/// grade graph.
fn subgraphs_canon(v: &mut Value) {
    match v {
        Value::Object(o) => {
            if let Some(sub) = o.get_mut("compound_subgraph").filter(|s| s.is_object()) {
                canon::grade_graph(sub);
            }
            o.values_mut().for_each(subgraphs_canon);
        }
        Value::Array(a) => a.iter_mut().for_each(subgraphs_canon),
        _ => {}
    }
}

/// Where `a` and `b` first differ: a path and the two values.
fn first_change(a: &Value, b: &Value, at: &str) -> String {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys().chain(y.keys()) {
                if x.get(k) != y.get(k) {
                    return first_change(x.get(k).unwrap_or(&Value::Null), y.get(k).unwrap_or(&Value::Null), &format!("{at}/{k}"));
                }
            }
            String::new()
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (p, q)) in x.iter().zip(y).enumerate() {
                if p != q {
                    return first_change(p, q, &format!("{at}/{i}"));
                }
            }
            String::new()
        }
        _ => format!("{at}: {a} vs {b}"),
    }
}

/// Whether a stored grade body holds a v1 compound with a subgraph.
fn has_compound_body(body: &Value) -> bool {
    body.get("nodes").and_then(Value::as_array).into_iter().flatten().any(|n| {
        n["legacy"]["payload"]["kind"] == "Compound" && n["legacy"]["payload"]["params"].get("compound_subgraph").is_some_and(|s| s.is_object())
            || n.get("group").and_then(|g| g.get("graph")).is_some_and(has_compound_body)
    })
}

/// Every node of `g`, its groups' bodies included.
fn all_nodes(g: &Graph) -> Vec<&Node> {
    let mut out = Vec::new();
    for n in &g.nodes {
        out.push(n);
        if let Some(b) = &n.group {
            out.extend(all_nodes(&b.graph));
        }
    }
    out
}

/// A picture-truth clip's revision-3 picture as revision 4 holds it, in
/// memory for the writer: its Speed Ramps' dropped fields gone, a Fixed
/// placement's aspect kept (declared), its plug-ins in their own nodes
/// (the writer's store makes those). Anything else a row changes is
/// refused, naming the clip and the row.
pub fn truth_picture(clip: &str, mut p: ClipPicture, declared: &mut Vec<Declared>) -> Result<ClipPicture, String> {
    for n in all_nodes(&p.graph) {
        let payload = n.legacy.as_ref().map(|l| &l.payload);
        if n.type_id.as_str() == types::LEGACY_NODE && payload.is_some_and(|v| v["kind"] == "Compound" && v["params"].get("compound_subgraph").is_some_and(|s| s.is_object())) {
            return Err(format!("clip {clip}: a picture-truth clip holds a grade compound (#1391)"));
        }
        if payload.is_some_and(|v| v["type"] == "speed_ramp" && v["freeze"] == true) {
            return Err(format!("clip {clip}: a picture-truth clip holds a frozen Speed Ramp (#1409)"));
        }
    }
    let opacities = p.graph.nodes.iter().filter(|n| n.type_id.as_str() == types::LEGACY_OPACITY).count();
    if opacities > 1 {
        return Err(format!("clip {clip}: a picture-truth clip holds {opacities} Opacity entries (#1386)"));
    }
    for n in &mut p.graph.nodes {
        if let Some(o) = n.legacy.as_mut().and_then(|l| l.payload.as_object_mut()).filter(|o| o.get("type").and_then(Value::as_str) == Some("speed_ramp")) {
            for f in DROPPED_RAMP_FIELDS {
                o.remove(f);
            }
        }
    }
    let fixed = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::PLACEMENT).and_then(|n| n.legacy.as_ref()).map(|l| l.payload.clone());
    if let Some(t) = fixed {
        let num = |k: &str| t.get(k).and_then(Value::as_f64).map(|x| x as f32);
        let aspect = crate::names::aspect(num("scale_x"), num("scale_y"));
        if p.placement.aspect.is_none() {
            p.placement.aspect = aspect;
        }
    }
    // Every revision-3 render drew a uniform scale (#1406), so a picture
    // that draws an aspect now is declared, stored before or added here.
    if let Some(a) = p.placement.aspect.filter(|a| *a != 1.0) {
        declared.push(Declared { row: "#1406", what: format!("clip {clip}: its Scale Y apart from its width now draws (aspect {a:.4})") });
    }
    crate::effect_params::picture_into_memory(&mut p)?;
    Ok(p)
}

/// The v1 project's Speed Ramp entries as revision 4 keeps them (#1409):
/// the fields that never drew dropped, and a frozen one, which never
/// blended, at speed 1 without blending, declared. The clip's own speed
/// stays as stored.
fn ramps(v1: &mut Value, declared: &mut Vec<Declared>) {
    for seq in objects(v1.get_mut("sequences")) {
        for track in objects(seq.get_mut("tracks")) {
            let track_id = id_of(track);
            if let Some(fx) = track.get_mut("effects").and_then(Value::as_object_mut) {
                for e in objects(fx.get_mut("video")) {
                    ramp(e, &format!("track {track_id}"), declared);
                }
            }
            for clip in objects(track.get_mut("clips")) {
                let clip_id = id_of(clip);
                for e in objects(clip.get_mut("builtin_effects")) {
                    ramp(e, &format!("clip {clip_id}"), declared);
                }
            }
        }
    }
}

fn ramp(e: &mut Map<String, Value>, whose: &str, declared: &mut Vec<Declared>) {
    if e.get("type").and_then(Value::as_str) != Some("speed_ramp") {
        return;
    }
    if e.get("freeze") == Some(&Value::Bool(true)) {
        e.insert("speed".into(), Value::from(1.0));
        e.insert("blend_mode".into(), Value::from(0));
        declared.push(Declared {
            row: "#1409",
            what: format!("{whose}: a frozen Speed Ramp (a 0.1x slow motion) becomes speed 1 without frame blending; the clip's own speed stays as stored"),
        });
    }
    for f in DROPPED_RAMP_FIELDS {
        e.remove(f);
    }
}

/// The changes the revision-4 lift made that change what a clip draws,
/// read from each clip's picture before (`before`, revision 3) and after
/// (in `v4`), and from the lift's notes.
fn declare_lifted(v4: &Value, before: &BTreeMap<(String, String), ClipPicture>, notes: &[LiftNote], declared: &mut Vec<Declared>) -> Result<(), String> {
    for seq in v4.get("sequences").and_then(Value::as_array).into_iter().flatten() {
        let seq_id = seq.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
        for track in seq.get("tracks").and_then(Value::as_array).into_iter().flatten() {
            for clip in track.get("clips").and_then(Value::as_array).into_iter().flatten() {
                let (Some(id), Some(stored)) = (clip.get("id").and_then(Value::as_str), clip.get(V2_FIELD)) else { continue };
                let after: ClipPicture = serde_json::from_value(stored.clone()).map_err(|e| format!("clip {id}: its revision-4 picture doesn't read: {e}"))?;
                if after.is_truth() {
                    continue;
                }
                let was = before.get(&(seq_id.clone(), id.to_owned()));
                // Every revision-3 render drew a uniform scale, whatever the
                // picture stored (#1479 stored the aspect before #1492 drew
                // it), so a picture that draws an aspect now is declared.
                if after.placement.aspect.is_some_and(|a| a != 1.0) {
                    declared.push(Declared { row: "#1406", what: format!("clip {id}: its Scale Y apart from its width now draws (aspect {:.4})", after.placement.aspect.unwrap_or(1.0)) });
                }
                let multiplied = after.graph.nodes.iter().filter(|n| n.type_id.as_str() == types::LEGACY_OPACITY && n.enabled).count();
                if multiplied > 0 {
                    declared.push(Declared { row: "#1386", what: format!("clip {id}: {multiplied} more enabled Opacity multiply its opacity") });
                } else if was.is_some_and(|w| (w.opacity - after.opacity).abs() > f32::EPSILON) {
                    declared.push(Declared { row: "#1386", what: format!("clip {id}: its opacity is its last enabled Opacity, {} where it was {}", after.opacity, was.map_or(1.0, |w| w.opacity)) });
                }
            }
        }
    }
    for n in notes {
        if let LiftNote::CompoundKeptOff { node, why } = n {
            declared.push(Declared { row: "#1391", what: format!("compound {node} is kept off: {why}, as it rendered before") });
        }
    }
    Ok(())
}


/// [`convert`] on a project's JSON: its revision-4 JSON, for a reader that
/// steps a copy as the converter does (the render check's `pixel_check`).
pub fn project(raw: &Value, catalog: &dyn klipp_graph_format::PortCatalog) -> Result<Value, String> {
    let text = serde_json::to_string(raw).map_err(err)?;
    serde_json::from_str(&convert(&text, catalog)?.text).map_err(err)
}
