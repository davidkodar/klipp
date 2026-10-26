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
use crate::to_v2;
use klipp_graph_format::lifted::{ClipPicture, LiftNote};
use klipp_graph_format::Graph;

/// The side stores' revision (Looks, stills, colour presets), and the
/// project's in revision 2.
pub const REVISION: u64 = 2;

/// The project's revision now: 3, where each effect's node id is its
/// identity, kept through a reorder and a save (M3 proposal 6). A project
/// converted from revision 0 or 1 has its effects' slot ids, which is what
/// revision 3 reads them as, so it is written as 3 directly. 4 is picture
/// revision 4 (M4 step F): a grade compound a group, an uneven scale's
/// aspect, every enabled Opacity multiplying, a Speed Ramp without the
/// fields that never drew, frei0r and OFX plug-ins as their own nodes; a
/// revision-3 project is converted once (`klipp-convert` step 4). 5 keeps
/// a tracked surface's keys as maps from its reference corners (#2754); a
/// revision-4 project is converted once (`klipp-convert` step 5).
pub const PROJECT_REVISION: u64 = 5;

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

/// One clip's picture lifted, as [`project`] lifts it, and its versions'
/// bodies: `clip` is the clip's saved record, of which only the picture
/// fields, `id` and the kind's `type` are read. A lift that would lose
/// something is refused, naming what.
pub fn lift_picture(clip: &Map<String, Value>) -> Result<(ClipPicture, Vec<Value>), String> {
    let mut notes = Vec::new();
    let lifted = lift_clip_store(clip, &mut notes)?;
    match notes.iter().find(|n| is_lost(n)) {
        Some(n) => Err(format!("lifting it would lose part of it ({n:?})")),
        None => Ok(lifted),
    }
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

fn lift_sequence(seq: &Map<String, Value>, notes: &mut Vec<LiftNote>, stored: StoredPictures<'_>) -> Result<Lifted, String> {
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
            let c = c.as_object().ok_or("a clip isn't an object")?;
            let seq_id = seq.get("id").and_then(Value::as_str).unwrap_or_default();
            match c.get("id").and_then(Value::as_str).and_then(|id| stored(seq_id, id)) {
                Some(picture) => clips.push(picture),
                None => clips.push(lift_clip_store(c, notes)?),
            }
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

/// Every clip of `seq` whose baked graph would let one part's wireless
/// sender take over another part's link (#1102 review). Today's bake merges
/// the group's pre stage, the clip's own grade, the post stage and the
/// track's grade into one graph, one namespace; in revision 2 each body
/// keeps its links to itself. Any two of those parts sharing a name, with a
/// sender on either side (a sender and a receiver, or two senders), is
/// noted. `group` is the clip's grade group, or nil where only the clip and
/// its track's grade are merged.
pub fn cross_group_wireless(seq: &Map<String, Value>) -> Vec<LiftNote> {
    use std::collections::BTreeSet;
    let mut notes = Vec::new();
    let groups: Vec<&Value> = seq.get("grade_groups").and_then(Value::as_array).into_iter().flatten().collect();
    let id = |v: &Value| v.get("id").and_then(Value::as_str).and_then(|s| Uuid::parse_str(s).ok()).unwrap_or_default();
    for track in seq.get("tracks").and_then(Value::as_array).into_iter().flatten() {
        let track_grade = track.get("effects").and_then(|e| e.get("grade")).filter(|g| !g.is_null());
        for clip in track.get("clips").and_then(Value::as_array).into_iter().flatten() {
            let group = clip.get("grade_group").and_then(Value::as_str).and_then(|gid| groups.iter().find(|g| g.get("id").and_then(Value::as_str) == Some(gid)));
            let stage = |k: &str| group.and_then(|g| g.get(k)).filter(|s| !s.is_null());
            // The parts the bake merges, each with what it sends and receives.
            let parts: Vec<(BTreeSet<String>, BTreeSet<String>)> =
                [stage("pre"), clip.get("node_graph").filter(|g| !g.is_null()), stage("post"), track_grade].into_iter().flatten().map(link_names).collect();
            let mut names = BTreeSet::new();
            for (i, (si, ri)) in parts.iter().enumerate() {
                for (sj, rj) in &parts[i + 1..] {
                    names.extend(si.iter().filter(|n| sj.contains(*n) || rj.contains(*n)).cloned());
                    names.extend(sj.iter().filter(|n| ri.contains(*n)).cloned());
                }
            }
            if !names.is_empty() {
                notes.push(LiftNote::CrossGroupWireless { clip: id(clip), group: group.map(|g| id(g)).unwrap_or_default(), names: names.into_iter().collect() });
            }
        }
    }
    notes
}

/// The revision-2 project from a v1 project, and the lift's notes.
pub fn project(raw: &Value) -> Result<(Value, Vec<LiftNote>), String> {
    project_with(raw, &|_, _| None)
}

/// A clip's picture already lifted, by sequence id and clip id: the app's
/// cache (#1089, M1b 2b), used in place of lifting that clip again. By
/// both, because a duplicated sequence keeps its clips' ids.
pub type StoredPictures<'a> = &'a dyn Fn(&str, &str) -> Option<(ClipPicture, Vec<Value>)>;

/// [`project`], taking each clip's picture from `stored` where it has one.
pub fn project_with(raw: &Value, stored: StoredPictures<'_>) -> Result<(Value, Vec<LiftNote>), String> {
    let mut notes = Vec::new();
    let mut copy = raw.clone();
    for seq in sequences_mut(&mut copy).map_err(err)? {
        notes.extend(cross_group_wireless(seq));
        let lifted = lift_sequence(seq, &mut notes, stored)?;
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
                // Each effect's entry written from its values: the file
                // keeps the entry, the map is memory's (M3-2).
                let mut picture = picture;
                crate::effect_params::picture_into_stored(&mut picture)?;
                clip.insert(V2_FIELD.into(), serde_json::to_value(picture).map_err(err)?);
            }
        }
    }
    old_fields::write_old_fields(&mut copy).map_err(err)?;
    // Every converted project says the project revision, an empty one too:
    // the app reads that one only.
    copy.as_object_mut().ok_or("not a project")?.insert("format_revision".into(), PROJECT_REVISION.into());
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

/// Whether a lift note says something the revision-2 file can't bring
/// back. With the complete lift (#1089) nothing is lost: every entry keeps
/// a node and every removal is recorded, so `unlift` restores it. The
/// match is exhaustive so that a new kind of note needs a decision; a save
/// refuses to write a file over a lost note.
pub fn is_lost(note: &LiftNote) -> bool {
    match note {
        LiftNote::NeverRan { .. }
        | LiftNote::CompoundKeptOff { .. }
        | LiftNote::ParkedEdge { .. }
        | LiftNote::OrphanKeys { .. }
        | LiftNote::ExtraTransform { .. }
        | LiftNote::DroppedField { .. }
        | LiftNote::LoadMigration { .. }
        | LiftNote::DefaultPipeline { .. }
        | LiftNote::MissingOutput { .. }
        | LiftNote::EdgeDropped { .. }
        | LiftNote::EdgeRewired { .. }
        | LiftNote::CrossGroupWireless { .. } => false,
    }
}
