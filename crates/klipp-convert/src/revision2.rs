//! Revision 2: the lifted files the switch writes (#1070), as pure
//! functions, with the shape agreed on #1002.
//!
//! Project (`format_revision: 2`):
//! - a clip's `graph_v2` is its [`ClipPicture`];
//! - a track's `effects.graph_v2` is `{"grade": <body>?, "effects": […]}`,
//!   the grade migrated as today's loader migrates it;
//! - a sequence's `graph_v2` is its timeline grade's body (not migrated);
//! - a grade group's `graph_v2` is `{"pre": <body>?, "post": <body>?}`;
//! - a grade version's `graph_v2` is its body (migrated);
//! - the old fields are written as in revision 1 (sentinels, placeholders).
//!
//! Side stores carry their own `format_revision: 2`: a Look is
//! `{format_revision, id, name, graph_v2}`, the stills index is
//! `{format_revision, stills: [{…, graph_v2?}]}`, a colour preset is
//! `{format_revision, graph_v2}`; none keeps its old graph field, so an old
//! build's typed read fails and it leaves the file alone. Lift notes go to
//! the report, never into a file.

use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::lift::{lift_clip, lift_grade_only_noted, ClipKind, LiftCtx};
use crate::old_fields::{self, sequences_mut, V2_FIELD};
use crate::{revision, to_v2};
use klipp_graph_format::lifted::{ClipPicture, LiftNote};
use klipp_graph_format::Graph;

/// The project's revision in revision 2, and the side stores'.
pub const REVISION: u64 = 2;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn objects(v: Option<&mut Value>) -> impl Iterator<Item = &mut Map<String, Value>> {
    v.and_then(Value::as_array_mut).into_iter().flatten().filter_map(Value::as_object_mut)
}

fn track_has_picture(track: &Map<String, Value>) -> bool {
    let Some(e) = track.get("effects").filter(|e| !e.is_null()) else { return false };
    e.get("video").and_then(Value::as_array).is_some_and(|v| !v.is_empty()) || e.get("grade").is_some_and(|g| !g.is_null())
}

/// A grade-only store's lifted body: a timeline grade, a stage, a version.
fn body(g: &Graph, migrate: bool, notes: &mut Vec<LiftNote>) -> Result<Value, String> {
    let b = lift_grade_only_noted(g, migrate, notes)?;
    b.check_lifted_body().map_err(err)?;
    serde_json::to_value(b).map_err(err)
}

/// One store's lifted pictures, each by what the conversion attaches.
struct Lifted {
    timeline: Option<Value>,
    groups: Vec<Option<Value>>,
    tracks: Vec<(Option<Value>, Vec<(ClipPicture, Vec<Value>)>)>,
}

/// A clip's lifted picture and its versions' bodies.
fn lift_clip_store(clip: &Map<String, Value>, notes: &mut Vec<LiftNote>) -> Result<(ClipPicture, Vec<Value>), String> {
    let record = to_v2::clip_to_v2(clip).map_err(err)?;
    let clip_id = Uuid::parse_str(clip.get("id").and_then(Value::as_str).unwrap_or_default()).map_err(err)?;
    let kind = ClipKind::from_json(clip.get("kind"))?;
    let (picture, n) = lift_clip(&record, &LiftCtx { clip_id, kind })?;
    picture.graph.check_lifted(false).map_err(err)?;
    notes.extend(n);
    let mut versions = Vec::new();
    for (i, v) in clip.get("grade_versions").and_then(Value::as_array).into_iter().flatten().enumerate() {
        let g = to_v2::version_to_v2(clip_id, i, v.as_object().ok_or("a version isn't an object")?).map_err(err)?;
        versions.push(body(&g, true, notes)?);
    }
    Ok((picture, versions))
}

fn lift_track_store(track: &Map<String, Value>, notes: &mut Vec<LiftNote>) -> Result<Option<Value>, String> {
    if !track_has_picture(track) {
        return Ok(None);
    }
    let structural = to_v2::track_to_v2(track).map_err(err)?;
    // The track's grade is migrated as today's loader migrates it.
    let mut effects = Vec::new();
    let mut grade = None;
    for n in &structural.nodes {
        if n.type_id.as_str() == klipp_graph_format::types::LEGACY_GRADE {
            let only = Graph { nodes: vec![n.clone()], output: n.id, ..Graph::empty() };
            grade = Some(body(&only, true, notes)?);
        } else {
            effects.push(n.clone());
        }
    }
    let mut out = json!({ "effects": effects });
    if let Some(g) = grade {
        out["grade"] = g;
    }
    Ok(Some(out))
}

fn lift_sequence(seq: &Map<String, Value>, notes: &mut Vec<LiftNote>) -> Result<Lifted, String> {
    let timeline = match to_v2::timeline_to_v2(seq).map_err(err)? {
        Some(g) => Some(body(&g, false, notes)?),
        None => None,
    };
    let mut groups = Vec::new();
    for g in seq.get("grade_groups").and_then(Value::as_array).into_iter().flatten() {
        let (pre, post) = to_v2::stages_to_v2(g.as_object().ok_or("a group isn't an object")?).map_err(err)?;
        let mut stages = Map::new();
        if let Some(p) = pre {
            stages.insert("pre".into(), body(&p, false, notes)?);
        }
        if let Some(p) = post {
            stages.insert("post".into(), body(&p, false, notes)?);
        }
        groups.push((!stages.is_empty()).then_some(Value::Object(stages)));
    }
    let mut tracks = Vec::new();
    for t in seq.get("tracks").and_then(Value::as_array).into_iter().flatten() {
        let t = t.as_object().ok_or("a track isn't an object")?;
        let picture = lift_track_store(t, notes)?;
        let mut clips = Vec::new();
        for c in t.get("clips").and_then(Value::as_array).into_iter().flatten() {
            clips.push(lift_clip_store(c.as_object().ok_or("a clip isn't an object")?, notes)?);
        }
        tracks.push((picture, clips));
    }
    Ok(Lifted { timeline, groups, tracks })
}

/// A v1 graph's wireless link names: those it sends, those it receives.
fn link_names(g: &Value) -> (std::collections::BTreeSet<String>, std::collections::BTreeSet<String>) {
    let (mut send, mut recv) = (std::collections::BTreeSet::new(), std::collections::BTreeSet::new());
    for n in g.get("nodes").and_then(Value::as_array).into_iter().flatten() {
        if let Some(name) = n.get("output_link").and_then(Value::as_str) {
            send.insert(name.to_owned());
        }
        for name in n.get("input_links").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
            recv.insert(name.to_owned());
        }
    }
    (send, recv)
}

/// Every clip of `seq` whose wireless link names meet those of a graph
/// today's bake merges into its own (its grade group's stages, its track's
/// grade), with a sender on either side (#1102 review): the merged graph is
/// one namespace, so one side's sender can take over the other's receiver.
/// In revision 2 each body keeps its own links. `group` is the clip's grade
/// group, or nil where only the track's grade meets it.
pub fn cross_group_wireless(seq: &Map<String, Value>) -> Vec<LiftNote> {
    let mut notes = Vec::new();
    let groups: Vec<&Value> = seq.get("grade_groups").and_then(Value::as_array).into_iter().flatten().collect();
    let id = |v: &Value| v.get("id").and_then(Value::as_str).and_then(|s| Uuid::parse_str(s).ok()).unwrap_or_default();
    for track in seq.get("tracks").and_then(Value::as_array).into_iter().flatten() {
        let track_grade = track.get("effects").and_then(|e| e.get("grade")).filter(|g| !g.is_null());
        for clip in track.get("clips").and_then(Value::as_array).into_iter().flatten() {
            let group = clip.get("grade_group").and_then(Value::as_str).and_then(|gid| groups.iter().find(|g| g.get("id").and_then(Value::as_str) == Some(gid)));
            let (mut os, mut or) = (std::collections::BTreeSet::new(), std::collections::BTreeSet::new());
            let stages = group.into_iter().flat_map(|g| ["pre", "post"].into_iter().filter_map(move |k| g.get(k).filter(|s| !s.is_null())));
            for g in stages.chain(track_grade) {
                let (s, r) = link_names(g);
                os.extend(s);
                or.extend(r);
            }
            let (cs, cr) = clip.get("node_graph").filter(|g| !g.is_null()).map(link_names).unwrap_or_default();
            let names: Vec<String> = os
                .iter()
                .filter(|n| cs.contains(*n) || cr.contains(*n))
                .chain(cs.iter().filter(|n| os.contains(*n) || or.contains(*n)))
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            if !names.is_empty() {
                notes.push(LiftNote::CrossGroupWireless { clip: id(clip), group: group.map(|g| id(g)).unwrap_or_default(), names });
            }
        }
    }
    notes
}

/// The revision-2 project from a v1 project, and the lift's notes.
pub fn project(raw: &Value) -> Result<(Value, Vec<LiftNote>), String> {
    let mut notes = Vec::new();
    let mut copy = raw.clone();
    for seq in sequences_mut(&mut copy).map_err(err)? {
        notes.extend(cross_group_wireless(seq));
        let lifted = lift_sequence(seq, &mut notes)?;
        if let Some(t) = lifted.timeline {
            seq.insert(V2_FIELD.into(), t);
        }
        for (group, stages) in objects(seq.get_mut("grade_groups")).zip(lifted.groups) {
            if let Some(s) = stages {
                group.insert(V2_FIELD.into(), s);
            }
        }
        for (track, (picture, clips)) in objects(seq.get_mut("tracks")).zip(lifted.tracks) {
            if let (Some(p), Some(effects)) = (picture, track.get_mut("effects").and_then(Value::as_object_mut)) {
                effects.insert(V2_FIELD.into(), p);
            }
            for (clip, (picture, versions)) in objects(track.get_mut("clips")).zip(clips) {
                for (version, b) in objects(clip.get_mut("grade_versions")).zip(versions) {
                    version.insert(V2_FIELD.into(), b);
                }
                clip.insert(V2_FIELD.into(), serde_json::to_value(picture).map_err(err)?);
            }
        }
    }
    old_fields::write_old_fields(&mut copy).map_err(err)?;
    // Every converted project says revision 2, an empty one too: after the
    // switch the app reads revision 2 only.
    if revision::needed_revision(&copy) > REVISION {
        return Err("the converted project needs a revision newer than 2".into());
    }
    copy.as_object_mut().ok_or("not a project")?.insert("format_revision".into(), REVISION.into());
    Ok((copy, notes))
}

/// A Look in revision 2: its graph lifted, `node_graph` gone.
pub fn look(raw: &Value) -> Result<Value, String> {
    look_noted(raw).map(|(v, _)| v)
}

/// [`look`], and the lift's notes for the report.
pub fn look_noted(raw: &Value) -> Result<(Value, Vec<LiftNote>), String> {
    let mut notes = Vec::new();
    let o = raw.as_object().ok_or("a Look isn't an object")?;
    let g = to_v2::look_to_v2(o).map_err(err)?;
    let mut out = o.clone();
    out.remove("node_graph");
    out.insert("format_revision".into(), REVISION.into());
    out.insert(V2_FIELD.into(), body(&g, false, &mut notes)?);
    Ok((Value::Object(out), notes))
}

/// The stills index in revision 2: a wrapper with its revision, each
/// still's grade lifted into `graph_v2` and its `grade` gone.
pub fn stills(raw: &Value) -> Result<Value, String> {
    stills_noted(raw).map(|(v, _)| v)
}

/// [`stills`], and the lift's notes for the report.
pub fn stills_noted(raw: &Value) -> Result<(Value, Vec<LiftNote>), String> {
    let mut notes = Vec::new();
    let list = raw.as_array().ok_or("the stills index isn't a list")?;
    let mut out = Vec::with_capacity(list.len());
    for s in list {
        let o = s.as_object().ok_or("a still isn't an object")?;
        let mut entry = o.clone();
        entry.remove("grade");
        if let Some(g) = to_v2::still_to_v2(o).map_err(err)? {
            entry.insert(V2_FIELD.into(), body(&g, false, &mut notes)?);
        }
        out.push(Value::Object(entry));
    }
    Ok((json!({ "format_revision": REVISION, "stills": out }), notes))
}

/// A colour preset in revision 2: `{format_revision, graph_v2}`.
pub fn colour_preset(file_name: &str, raw: &Value) -> Result<Value, String> {
    colour_preset_noted(file_name, raw).map(|(v, _)| v)
}

/// [`colour_preset`], and the lift's notes for the report.
pub fn colour_preset_noted(file_name: &str, raw: &Value) -> Result<(Value, Vec<LiftNote>), String> {
    let g = to_v2::colour_preset_to_v2(file_name, raw.as_object().ok_or("a preset isn't an object")?).map_err(err)?;
    let mut notes = Vec::new();
    let v = json!({ "format_revision": REVISION, V2_FIELD: body(&g, false, &mut notes)? });
    Ok((v, notes))
}

/// Prove a written revision-2 project: it reads back at revision 2, its
/// sentinels strip, every store's `graph_v2` is exactly the lift of the
/// original store, and the rest of the project is the original's, field
/// by field.
pub fn verify_project(original: &Value, written: &str) -> Result<(), String> {
    let mut back: Value = serde_json::from_str(written).map_err(|e| format!("the written file doesn't read back: {e}"))?;
    let r = revision::read_revision(&back).map_err(err)?;
    if r != REVISION {
        return Err(format!("written with revision {r}, not {REVISION}"));
    }
    old_fields::strip_sentinels(&mut back).map_err(err)?;
    let (expected, _) = project(original)?;
    let mut expected = expected;
    old_fields::strip_sentinels(&mut expected).map_err(err)?;
    if back != expected {
        return Err(format!("the written project isn't the conversion of the original: {}", crate::verify::first_diff(&back, &expected, "").unwrap_or_default()));
    }
    let (o, b) = (crate::assemble::without_pictures(original), crate::assemble::without_pictures(&back));
    if o != b {
        return Err(format!("outside the pictures: {}", crate::verify::first_diff(&b, &o, "").unwrap_or_default()));
    }
    Ok(())
}

/// Prove a written revision-2 side store against its original: the same
/// conversion, and nothing else changed.
pub fn verify_side(converted_again: &Value, written: &str) -> Result<(), String> {
    let back: Value = serde_json::from_str(written).map_err(|e| format!("the written file doesn't read back: {e}"))?;
    if back.get("format_revision").and_then(Value::as_u64) != Some(REVISION) {
        return Err("a side store not at revision 2".into());
    }
    if &back != converted_again {
        return Err(format!("the written file isn't the conversion: {}", crate::verify::first_diff(&back, converted_again, "").unwrap_or_default()));
    }
    Ok(())
}

/// `o` without the keys a conversion replaces.
fn without(o: &Value, keys: &[&str]) -> Value {
    let mut m = o.as_object().cloned().unwrap_or_default();
    for k in keys {
        m.remove(*k);
    }
    Value::Object(m)
}

/// The rest of a written store against the original, outside the picture
/// fields: what no conversion may change.
fn same_rest(original: &Value, written: &Value, picture: &str, what: &str) -> Result<(), String> {
    let (o, w) = (without(original, &[picture]), without(written, &[V2_FIELD, "format_revision"]));
    if o != w {
        return Err(format!("{what}: outside the picture: {}", crate::verify::first_diff(&w, &o, "").unwrap_or_default()));
    }
    Ok(())
}

/// Prove a written revision-2 Look: the conversion of the original, and
/// outside its graph the original itself.
pub fn verify_look(original: &Value, written: &str) -> Result<(), String> {
    verify_side(&look(original)?, written)?;
    let back: Value = serde_json::from_str(written).map_err(err)?;
    same_rest(original, &back, "node_graph", "the Look")
}

/// Prove a written revision-2 stills index: the conversion of the
/// original, and every still, outside its grade, the original's.
pub fn verify_stills(original: &Value, written: &str) -> Result<(), String> {
    verify_side(&stills(original)?, written)?;
    let back: Value = serde_json::from_str(written).map_err(err)?;
    let (o, w) = (original.as_array().ok_or("the stills index isn't a list")?, back["stills"].as_array().ok_or("the written index has no list")?);
    if o.len() != w.len() {
        return Err(format!("{} stills written for {}", w.len(), o.len()));
    }
    for (i, (a, b)) in o.iter().zip(w).enumerate() {
        same_rest(&without(a, &["grade"]), &without(b, &[V2_FIELD]), "grade", &format!("still {i}"))?;
    }
    Ok(())
}

/// Prove a written revision-2 colour preset: the conversion of the
/// original, and nothing but its revision and its graph.
pub fn verify_colour_preset(file_name: &str, original: &Value, written: &str) -> Result<(), String> {
    verify_side(&colour_preset(file_name, original)?, written)?;
    let back: Value = serde_json::from_str(written).map_err(err)?;
    let extra: Vec<&String> = back.as_object().into_iter().flatten().map(|(k, _)| k).filter(|k| k.as_str() != V2_FIELD && k.as_str() != "format_revision").collect();
    if !extra.is_empty() {
        return Err(format!("the preset holds {extra:?} besides its graph"));
    }
    Ok(())
}
