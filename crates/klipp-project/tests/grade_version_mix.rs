//! A clip grade version's correction mix and mask channel (#1322) through a
//! real save and a strict reload: a version holding one stores it in its
//! body, which then names `grade-mix-1` in its `requires`, and reads back
//! with it; a version holding none is stored exactly as before, so a
//! project with no such version saves and reloads unchanged (its version
//! bodies byte for byte).

use klipp_effects::mask_mix::MaskChannel;
use klipp_graph_format::lifted::GRADE_MIX_CAPABILITY;
use klipp_media::MediaPool;
use klipp_nodes::params::EffectBlend;
use klipp_nodes::{NodeGraph, NodeKind};
use klipp_project::{load_project_strict, save_project};
use klipp_timeline::{Clip, GradeAccess, GradeVersion, Sequence};
use serde_json::Value;

fn tc(s: f64) -> klipp_core::RationalTime {
    klipp_core::RationalTime::from_secs_f64(s, 24)
}

/// A sequence with one clip and two grade versions of its grade, the
/// second's Corrector given `blend` as its run setting.
fn sequence(blend: Option<EffectBlend>) -> Sequence {
    let mut g = NodeGraph::default();
    g.ensure_corrector();
    let mut c = Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    c.set_grade(g.clone());
    let mut mixed = g.clone();
    mixed.nodes.iter_mut().find(|n| n.kind == NodeKind::Corrector).unwrap().params.correction_blend = blend;
    c.edit_grade_versions(|vs| vs.extend([GradeVersion::new(String::from("A"), g), GradeVersion::new(String::from("B"), mixed)]));
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips.push(c);
    seq
}

/// `sequences` saved: the file's text, and the sequences read back strictly.
fn saved(name: &str, sequences: &[Sequence]) -> (String, Vec<Sequence>) {
    let dir = std::env::temp_dir().join(format!("klipp_grade_version_mix_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    save_project(&dir, sequences, &MediaPool::new()).unwrap_or_else(|e| panic!("{name}: saving: {e}"));
    let text = std::fs::read_to_string(dir.join("project.json")).unwrap();
    let (loaded, ..) = load_project_strict(&dir).unwrap_or_else(|e| panic!("{name}: a strict reload: {e}"));
    let _ = std::fs::remove_dir_all(&dir);
    (text, loaded)
}

/// The stored bodies of the first clip's grade versions.
fn bodies(text: &str) -> Vec<Value> {
    let file: Value = serde_json::from_str(text).unwrap();
    file["sequences"][0]["tracks"][0]["clips"][0]["grade_versions"].as_array().unwrap().iter().map(|v| v["graph_v2"].clone()).collect()
}

/// The saved sequences, without the save's own stamps (its generation and
/// time), which every save changes.
fn content(text: &str) -> Value {
    serde_json::from_str::<Value>(text).unwrap()["sequences"].clone()
}

fn mixes(body: &Value) -> bool {
    body.to_string().contains("\"mix\"") || body.to_string().contains("mask.channel")
}

/// A project whose versions hold no mix or channel saves with none in
/// its bodies and no `requires`, reloads strictly, and saves again byte
/// for byte the same: nothing changes until a version holds one.
#[test]
fn versions_without_a_mix_save_and_reload_unchanged() {
    let (first, loaded) = saved("plain", &[sequence(None)]);
    for b in bodies(&first) {
        assert!(b.get("requires").is_none(), "no requires: {b}");
        assert!(!mixes(&b), "no mix or channel: {b}");
    }
    let (second, _) = saved("plain-again", &loaded);
    assert_eq!(bodies(&first).iter().map(Value::to_string).collect::<Vec<_>>(), bodies(&second).iter().map(Value::to_string).collect::<Vec<_>>(), "the version bodies byte for byte");
    assert_eq!(content(&first), content(&second), "saved again, the same sequences");
}

/// A version holding a mix and a channel stores them in its body, which
/// alone requires `grade-mix-1`; read back, the version holds them, and
/// saving again writes the same file.
#[test]
fn a_version_s_mix_is_stored_in_its_body_and_read_back() {
    let blend = EffectBlend { mix: 0.4, channel: MaskChannel::Alpha };
    let (first, loaded) = saved("mixed", &[sequence(Some(blend))]);
    let b = bodies(&first);
    assert!(b[0].get("requires").is_none() && !mixes(&b[0]), "version A holds none: {}", b[0]);
    assert_eq!(b[1]["requires"], serde_json::json!([GRADE_MIX_CAPABILITY]), "version B requires it: {}", b[1]);
    assert!(mixes(&b[1]), "version B's body holds it: {}", b[1]);
    let c = loaded[0].tracks[0].clips.first().unwrap();
    let read = c.grade_versions()[1].grade().nodes.iter().find(|n| n.kind == NodeKind::Corrector).unwrap().params.correction_blend;
    assert_eq!(read, Some(blend), "read back");
    assert!(c.version_blends(0).is_empty(), "A still holds none");
    let (second, _) = saved("mixed-again", &loaded);
    assert_eq!(content(&first), content(&second), "saved again, the same sequences");
}
