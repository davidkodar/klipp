//! The whole converted project: every store's converted picture attached
//! where it lives, the old fields written for old builds, and the
//! revision its content needs (#1066). The real run writes exactly what
//! [`to_revision1`] builds; the dry run builds it in memory and proves it
//! with [`verify`].
//!
//! Where each store's converted picture lives (`graph_v2`):
//! - a clip: on the clip, its [`ClipRecordV2`];
//! - a track: on the track's `effects`, its graph;
//! - a sequence's timeline grade: on the sequence;
//! - a grade group: on the group, `{"pre": graph?, "post": graph?}`;
//! - a grade version: on the version.

use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::canon::{canon, Store};
use crate::old_fields::{self, sequences_mut, V2_FIELD};
use crate::{revision, to_v1, to_v2};
use klipp_graph_format::{ClipRecordV2, Graph};

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn objects(v: Option<&mut Value>) -> impl Iterator<Item = &mut Map<String, Value>> {
    v.and_then(Value::as_array_mut).into_iter().flatten().filter_map(Value::as_object_mut)
}

fn non_null<'a>(o: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    o.get(key).filter(|v| !v.is_null())
}

/// Whether a track's effects hold picture data: a video effect or a grade.
fn track_has_picture(track: &Map<String, Value>) -> bool {
    let Some(e) = non_null(track, "effects") else { return false };
    e.get("video").and_then(Value::as_array).is_some_and(|v| !v.is_empty()) || e.get("grade").is_some_and(|g| !g.is_null())
}

/// Build the converted project from a v1 project: attach every store's
/// converted picture, write the old fields and set the revision.
pub fn to_revision1(raw: &Value) -> Result<Value, String> {
    let mut copy = raw.clone();
    for seq in sequences_mut(&mut copy).map_err(err)? {
        if let Some(g) = to_v2::timeline_to_v2(seq).map_err(err)? {
            seq.insert(V2_FIELD.into(), serde_json::to_value(g).map_err(err)?);
        }
        for group in objects(seq.get_mut("grade_groups")) {
            let (pre, post) = to_v2::stages_to_v2(group).map_err(err)?;
            if pre.is_some() || post.is_some() {
                group.insert(V2_FIELD.into(), json!({"pre": pre, "post": post}));
            }
        }
        for track in objects(seq.get_mut("tracks")) {
            if track_has_picture(track) {
                let g = to_v2::track_to_v2(track).map_err(err)?;
                if let Some(effects) = track.get_mut("effects").and_then(Value::as_object_mut) {
                    effects.insert(V2_FIELD.into(), serde_json::to_value(g).map_err(err)?);
                }
            }
            for clip in objects(track.get_mut("clips")) {
                let record = to_v2::clip_to_v2(clip).map_err(err)?;
                let id = Uuid::parse_str(clip.get("id").and_then(Value::as_str).unwrap_or_default()).map_err(err)?;
                for (n, version) in objects(clip.get_mut("grade_versions")).enumerate() {
                    let g = to_v2::version_to_v2(id, n, version).map_err(err)?;
                    version.insert(V2_FIELD.into(), serde_json::to_value(g).map_err(err)?);
                }
                clip.insert(V2_FIELD.into(), serde_json::to_value(record).map_err(err)?);
            }
        }
    }
    old_fields::write_old_fields(&mut copy).map_err(err)?;
    revision::write_revision(&mut copy);
    Ok(copy)
}

/// The sequences of a project, read-only, in both shapes.
fn sequences(v: &Value) -> Vec<&Value> {
    match v.get("sequences") {
        Some(list) => list.as_array().map(|l| l.iter().collect()).unwrap_or_default(),
        None => v.get("sequence").into_iter().collect(),
    }
}

fn items<'a>(v: &'a Value, key: &str) -> Vec<&'a Value> {
    v.get(key).and_then(Value::as_array).map(|l| l.iter().collect()).unwrap_or_default()
}

fn graph_of(v: &Value) -> Result<Graph, String> {
    let g: Graph = serde_json::from_value(v.clone()).map_err(err)?;
    g.check().map_err(err)?;
    Ok(g)
}

/// The written graph must be exactly what converting the original store
/// gives: payloads, order, edges, output, version, flags. Checking the
/// inverse alone would miss a changed edge or output, which `to_v1` reads
/// past.
fn exact<T: PartialEq + serde::Serialize>(what: &str, written: &T, expected: &T) -> Result<(), String> {
    if written == expected {
        Ok(())
    } else {
        Err(format!("{what}: the written graph isn't the conversion of the original: {}", crate::verify::graph_diff(written, expected)))
    }
}

fn obj(v: &Value) -> Result<&Map<String, Value>, String> {
    v.as_object().ok_or_else(|| "not an object".to_owned())
}

/// A picture as read back from the written file must be the original's
/// picture, by `canon()`'s equality.
fn same(what: String, back: Value, original: Value) -> Result<(), String> {
    if back == original {
        Ok(())
    } else {
        Err(format!("{what}: {}", crate::verify::first_diff(&back, &original, "").unwrap_or_default()))
    }
}

/// Picture fields a written file holds differently, removed from both
/// sides before the rest of the project is compared byte for byte.
const CLIP_PICTURE: [&str; 14] = [
    "node_graph",
    "effects",
    "builtin_effects",
    "builtin_effects_enabled",
    "effect_keyframes",
    "opacity_keyframes",
    "transform_pos_x_keyframes",
    "transform_pos_y_keyframes",
    "transform_scale_keyframes",
    "transform_scale_y_keyframes",
    "transform_rotation_keyframes",
    "transform_tilt_x_keyframes",
    "transform_tilt_y_keyframes",
    V2_FIELD,
];
const AUDIO_MODIFIERS: [&str; 3] = ["volume", "pan", "gain"];

/// The project with every picture field removed, so what remains must be
/// identical before and after conversion: names, timing, sound, markers,
/// everything that isn't the picture.
pub(crate) fn without_pictures(v: &Value) -> Value {
    let mut v = v.clone();
    if let Some(o) = v.as_object_mut() {
        o.remove("format_revision");
    }
    let Ok(seqs) = sequences_mut(&mut v) else { return v };
    for seq in seqs {
        seq.remove("timeline_graph");
        seq.remove(V2_FIELD);
        for group in objects(seq.get_mut("grade_groups")) {
            for k in ["pre", "post", V2_FIELD] {
                group.remove(k);
            }
        }
        for track in objects(seq.get_mut("tracks")) {
            if let Some(effects) = track.get_mut("effects").and_then(Value::as_object_mut) {
                for k in ["video", "video_enabled", "grade", V2_FIELD] {
                    effects.remove(k);
                }
                if effects.is_empty() {
                    track.remove("effects");
                }
            }
            for clip in objects(track.get_mut("clips")) {
                for k in CLIP_PICTURE {
                    clip.remove(k);
                }
                if let Some(m) = clip.get_mut("param_modifiers").and_then(Value::as_object_mut) {
                    m.retain(|k, _| AUDIO_MODIFIERS.contains(&k.as_str()));
                }
                if clip.get("param_modifiers").is_some_and(|m| m.as_object().is_none_or(Map::is_empty)) {
                    clip.remove("param_modifiers");
                }
                for version in objects(clip.get_mut("grade_versions")) {
                    version.remove("node_graph");
                    version.remove(V2_FIELD);
                }
            }
        }
    }
    v
}

/// Prove a written converted project: read it back from its text, check
/// the revision, strip the sentinels, rebuild every store's v1 picture
/// from `graph_v2` and compare it with the original's, and compare the
/// rest of the project field by field.
pub fn verify(original: &Value, written: &str) -> Result<(), String> {
    let mut back: Value = serde_json::from_str(written).map_err(|e| format!("the written file doesn't read back: {e}"))?;
    let r = revision::read_revision(&back).map_err(err)?;
    let needed = revision::needed_revision(&back);
    if r != needed {
        return Err(format!("written with revision {r}, the content needs {needed}"));
    }
    old_fields::strip_sentinels(&mut back).map_err(err)?;
    let (orig_seqs, back_seqs) = (sequences(original), sequences(&back));
    if orig_seqs.len() != back_seqs.len() {
        return Err("the number of sequences changed".into());
    }
    for (si, (o, b)) in orig_seqs.iter().zip(&back_seqs).enumerate() {
        match (o.get("timeline_graph").filter(|g| !g.is_null()), b.get(V2_FIELD)) {
            (None, None) => {}
            (Some(g), Some(v2)) => {
                let written = graph_of(v2)?;
                let expected = to_v2::timeline_to_v2(obj(o)?).map_err(err)?.ok_or("no timeline grade")?;
                exact(&format!("sequence {si} timeline grade"), &written, &expected)?;
                same(format!("sequence {si} timeline grade"), to_v1::grade_only(&written).map_err(err)?, canon(Store::Graph, g))?
            }
            _ => return Err(format!("sequence {si}: the timeline grade was lost or invented")),
        }
        for (gi, (og, bg)) in items(o, "grade_groups").into_iter().zip(items(b, "grade_groups")).enumerate() {
            let (pre, post) = to_v2::stages_to_v2(obj(og)?).map_err(err)?;
            for (stage, expected) in [("pre", pre), ("post", post)] {
                let orig = og.get(stage).filter(|g| !g.is_null());
                let conv = bg.get(V2_FIELD).and_then(|v| v.get(stage)).filter(|g| !g.is_null());
                match (orig, conv) {
                    (None, None) => {}
                    (Some(g), Some(v2)) => {
                        let written = graph_of(v2)?;
                        exact(&format!("sequence {si} group {gi} {stage}"), &Some(written.clone()), &expected)?;
                        same(format!("sequence {si} group {gi} {stage}"), to_v1::grade_only(&written).map_err(err)?, canon(Store::Graph, g))?
                    }
                    _ => return Err(format!("sequence {si} group {gi}: the {stage} stage was lost or invented")),
                }
            }
        }
        let (ot, bt) = (items(o, "tracks"), items(b, "tracks"));
        if ot.len() != bt.len() {
            return Err(format!("sequence {si}: the number of tracks changed"));
        }
        for (ti, (otr, btr)) in ot.into_iter().zip(bt).enumerate() {
            let orig_effects = otr.get("effects").filter(|e| !e.is_null()).cloned().unwrap_or(json!({}));
            let back_picture = match btr.get("effects").and_then(|e| e.get(V2_FIELD)) {
                Some(v2) => {
                    let written = graph_of(v2)?;
                    exact(&format!("sequence {si} track {ti}"), &written, &to_v2::track_to_v2(obj(otr)?).map_err(err)?)?;
                    to_v1::track(&written).map_err(err)?
                }
                None => json!({}),
            };
            same(format!("sequence {si} track {ti}"), back_picture, canon(Store::Track, &orig_effects))?;
            let (oc, bc) = (items(otr, "clips"), items(btr, "clips"));
            if oc.len() != bc.len() {
                return Err(format!("sequence {si} track {ti}: the number of clips changed"));
            }
            for (ci, (oclip, bclip)) in oc.into_iter().zip(bc).enumerate() {
                let at = format!("sequence {si} track {ti} clip {ci}");
                let record = bclip.get(V2_FIELD).ok_or_else(|| format!("{at}: no converted picture"))?;
                let written: ClipRecordV2 = serde_json::from_value(record.clone()).map_err(|e| format!("{at}: {e}"))?;
                written.check().map_err(|e| format!("{at}: {e}"))?;
                exact(&at, &written, &to_v2::clip_to_v2(obj(oclip)?).map_err(err)?)?;
                let clip_id = Uuid::parse_str(oclip.get("id").and_then(Value::as_str).unwrap_or_default()).map_err(err)?;
                same(at.clone(), to_v1::clip(record).map_err(err)?, canon(Store::Clip, oclip))?;
                for (vi, (ov, bv)) in items(oclip, "grade_versions").into_iter().zip(items(bclip, "grade_versions")).enumerate() {
                    let v2 = bv.get(V2_FIELD).ok_or_else(|| format!("{at} version {vi}: no converted picture"))?;
                    let og = ov.get("node_graph").cloned().unwrap_or(Value::Null);
                    let written = graph_of(v2)?;
                    exact(&format!("{at} version {vi}"), &written, &to_v2::version_to_v2(clip_id, vi, obj(ov)?).map_err(err)?)?;
                    same(format!("{at} version {vi}"), to_v1::grade_only(&written).map_err(err)?, canon(Store::Graph, &og))?;
                }
            }
        }
    }
    let (o, b) = (without_pictures(original), without_pictures(&back));
    if o != b {
        return Err(format!("outside the pictures: {}", crate::verify::first_diff(&b, &o, "").unwrap_or_default()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIP: &str = "00000000-0000-0000-0000-000000000c01";
    const TRACK: &str = "00000000-0000-0000-0000-000000000701";
    const SEQ: &str = "00000000-0000-0000-0000-000000000501";
    const GROUP: &str = "00000000-0000-0000-0000-000000000601";
    const N: &str = "00000000-0000-0000-0000-0000000000a1";

    fn grade() -> Value {
        json!({"nodes": [{"id": N, "kind": "Exposure", "label": "E", "enabled": true, "params": {"exposure": 0.5}}],
               "edges": [], "output_node_id": N})
    }

    fn sequence() -> Value {
        let crop = json!({"type": "crop", "left": 0.1, "right": 0.0, "top": 0.0, "bottom": 0.0, "feather": 0.0});
        json!({
            "id": SEQ, "name": "Main", "timeline_graph": grade(),
            "grade_groups": [{"id": GROUP, "name": "Ext", "pre": grade(), "departs": true}],
            "markers": [{"name": "m"}],
            "tracks": [{
                "id": TRACK, "name": "V1", "kind": "Video",
                "effects": {"video": [crop.clone()], "grade": grade(), "audio": [{"kind": "eq", "gain": 2.0}]},
                "clips": [{
                    "id": CLIP, "name": "shot", "timeline_in": {"value": 0, "rate": 24},
                    "node_graph": grade(), "builtin_effects": [crop], "effects": [],
                    "param_modifiers": {"volume": {"modifiers": []}, "opacity": {"modifiers": []}},
                    "volume_keyframes": [{"time_secs": 0.0, "value": 1.0}],
                    "grade_versions": [{"name": "Warm", "node_graph": grade()}]
                }]
            }]
        })
    }

    fn project() -> Value {
        json!({"version": "1.0.0", "notes": "keep me", "sequences": [sequence()]})
    }

    fn written(p: &Value) -> String {
        serde_json::to_string(&to_revision1(p).unwrap()).unwrap()
    }

    #[test]
    fn every_store_converts_and_reads_back_whole() {
        let p = project();
        let text = written(&p);
        verify(&p, &text).unwrap();
        let back: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(back["format_revision"], 1);
        let seq = &back["sequences"][0];
        assert!(seq.get("timeline_graph").is_none() && seq.get(V2_FIELD).is_some());
        assert!(seq["grade_groups"][0][V2_FIELD]["pre"].is_object());
        assert!(seq["grade_groups"][0][V2_FIELD]["post"].is_null());
        assert_eq!(seq["tracks"][0]["effects"]["audio"], json!([{"kind": "eq", "gain": 2.0}]), "the track's sound stays");
        assert_eq!(seq["tracks"][0]["clips"][0]["grade_versions"][0]["node_graph"], old_fields::placeholder_graph());
    }

    #[test]
    fn the_oldest_single_sequence_shape_converts_too() {
        let p = json!({"version": "0.1", "sequence": sequence()});
        let text = written(&p);
        verify(&p, &text).unwrap();
        let back: Value = serde_json::from_str(&text).unwrap();
        assert!(back.get("sequences").is_none(), "the file keeps its shape");
        assert_eq!(back["format_revision"], 1);
    }

    #[test]
    fn a_timeline_grade_alone_needs_the_revision() {
        let p = json!({"version": "1.0.0", "sequences": [{"id": SEQ, "name": "S", "timeline_graph": grade(), "tracks": []}]});
        let back: Value = serde_json::from_str(&written(&p)).unwrap();
        assert_eq!(back["format_revision"], 1);
        verify(&p, &serde_json::to_string(&back).unwrap()).unwrap();
    }

    /// The proof fails on a lost store, a changed picture, or any change
    /// outside the pictures.
    #[test]
    fn a_damaged_copy_is_caught() {
        let p = project();
        let good: Value = serde_json::from_str(&written(&p)).unwrap();
        let damage = |f: &dyn Fn(&mut Value)| {
            let mut v = good.clone();
            f(&mut v);
            verify(&p, &serde_json::to_string(&v).unwrap())
        };
        assert!(damage(&|v| { v["sequences"][0]["grade_groups"][0].as_object_mut().unwrap().remove(V2_FIELD); }).is_err());
        assert!(damage(&|v| { v["sequences"][0].as_object_mut().unwrap().remove(V2_FIELD); }).is_err());
        assert!(damage(&|v| { v["sequences"][0]["tracks"][0]["clips"][0]["grade_versions"][0].as_object_mut().unwrap().remove(V2_FIELD); }).is_err());
        assert!(damage(&|v| v["sequences"][0]["tracks"][0]["effects"]["audio"] = json!([])).is_err());
        assert!(damage(&|v| v["notes"] = json!("changed")).is_err());
        assert!(damage(&|v| v["sequences"][0]["tracks"][0]["clips"][0]["param_modifiers"]["volume"] = json!({"x": 1})).is_err());
        assert!(damage(&|v| v["format_revision"] = json!(0)).is_err());
        // Structure the inverse reads past: edges, output, version.
        assert!(damage(&|v| v["sequences"][0]["tracks"][0]["clips"][0][V2_FIELD]["graph"]["edges"] = json!([])).is_err());
        assert!(damage(&|v| v["sequences"][0][V2_FIELD]["graph_version"] = json!(999)).is_err());
        assert!(damage(&|v| {
            let g = &mut v["sequences"][0]["tracks"][0]["clips"][0][V2_FIELD]["graph"];
            let first = g["nodes"][0]["id"].clone();
            g["output"] = first;
        }).is_err());
        assert!(damage(&|v| v["sequences"][0]["tracks"][0]["clips"][0][V2_FIELD]["graph"]["nodes"][1]["enabled"] = json!(false)).is_err());
        let mut picture = good.clone();
        picture["sequences"][0]["tracks"][0]["clips"][0][V2_FIELD]["graph"]["nodes"][0]["group"]["graph"]["nodes"][0]["legacy"]["payload"]["params"]["exposure"] = json!(0.25);
        assert!(verify(&p, &serde_json::to_string(&picture).unwrap()).is_err());
    }
}
