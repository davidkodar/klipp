//! A picture-truth clip (node-graph M2 step 9c) in the saved file: its
//! picture, holding a mask published from its grade and a mixed effect, is
//! saved as the clip's stored picture with `requires: ["picture-truth-1"]`,
//! read back as the clip's truth, and saved again the same, byte for byte.
//! Only such a clip carries the mark. A picture that requires what this
//! Klipp doesn't know is refused, and the file left as it was.

use klipp_core::{MediaId, RationalTime};
use klipp_effects::BuiltinEffect as E;
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::PortBinding;
use klipp_graph_lift::ops::GraphOp;
use klipp_media::MediaPool;
use klipp_nodes::{Edge, Node, NodeGraph, NodeKind, NodeParams};
use klipp_project::{load_project, save_project};
use klipp_timeline::{Clip, GradeAccess, Sequence};
use std::path::{Path, PathBuf};

/// The top-level lines a save stamps afresh every time.
const STAMPS: [&str; 4] = ["  \"generation\":", "  \"saved_at\":", "  \"klipp_version\":", "  \"platform\":"];

fn project_text(dir: &Path) -> String {
    let text = std::fs::read_to_string(dir.join("project.json")).unwrap();
    text.lines().filter(|l| !STAMPS.iter().any(|s| l.starts_with(s))).collect::<Vec<_>>().join("\n")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_picture_truth_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A media clip with a blur and a vignette, its grade's qualifier published
/// as a matte that masks the blur, mixed at a half; and a plain clip beside
/// it.
fn sequence() -> (Sequence, Clip, Clip) {
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut c = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    c.push_effect(E::GaussianBlur { radius: 3.0 });
    c.push_effect(E::Vignette { amount: 0.5, size: 0.5, softness: 0.5, roundness: 0.5 });
    let mut g = NodeGraph::default();
    g.ensure_corrector();
    let source = g.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id;
    let qualifier = Node { id: uuid::Uuid::new_v4(), kind: NodeKind::Qualifier, label: String::new(), enabled: true, params: NodeParams::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() };
    g.edges.push(Edge { from_node: source, from_port: 0, to_node: qualifier.id, to_port: 0 });
    let q = qualifier.id;
    g.nodes.push(qualifier);
    c.set_grade(g);
    let p = c.picture_v2().unwrap().picture.clone();
    let group = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).unwrap().id;
    let blur = p.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::types::LEGACY_EFFECT).unwrap().id;
    let at = |id| NodePath::new(Root::Clip { clip: c.id.0 }, [id]);
    c.apply_picture_ops(&[
        GraphOp::PublishOutput { group: at(group), endpoint: PortBinding { node: q, port: "out".into() }, key: "matte_selection".into(), label: "Selection".into() },
        GraphOp::Connect { from: at(group), from_port: "matte_selection".into(), to: at(blur), to_port: "mask".into() },
        GraphOp::SetMix { path: at(blur), mix: Some(0.5) },
    ])
    .unwrap();
    assert!(c.is_picture_truth());
    let plain = Clip::media(MediaId::new(), t(4.0), t(8.0), t(0.0), t(4.0));
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips = vec![c.clone(), plain.clone()];
    (seq, c, plain)
}

fn clips(json: &serde_json::Value) -> Vec<serde_json::Value> {
    json["sequences"][0]["tracks"][0]["clips"].as_array().unwrap().clone()
}

#[test]
fn a_picture_truth_clip_saves_reopens_and_saves_again_the_same() {
    let (seq, masked, plain) = sequence();
    let first = scratch("first");
    save_project(&first, std::slice::from_ref(&seq), &MediaPool::new()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(first.join("project.json")).unwrap()).unwrap();
    let saved = clips(&json);
    assert_eq!(saved[0]["graph_v2"]["requires"], serde_json::json!(["picture-truth-1"]), "the picture-truth clip is marked");
    assert!(saved[1]["graph_v2"].get("requires").is_none(), "a clip whose fields are its truth isn't");

    let loaded = load_project(&first).unwrap();
    let reopened = loaded.0[0].tracks[0].clips[0].clone();
    assert!(reopened.is_picture_truth(), "read back as its truth");
    assert_eq!(reopened.picture_v2().unwrap().picture, masked.picture_v2().unwrap().picture, "the picture as saved: every id, the mask, the mix");
    assert_eq!(reopened.builtin_effects(), masked.builtin_effects(), "and the fields, its view");
    assert!(!loaded.0[0].tracks[0].clips[1].is_picture_truth());
    assert_eq!(loaded.0[0].tracks[0].clips[1].picture_v2().unwrap().picture, plain.picture_v2().unwrap().picture);

    let second = scratch("second");
    save_project(&second, &loaded.0, &loaded.1).unwrap();
    assert_eq!(project_text(&second), project_text(&first), "saved again byte for byte");
    let _ = std::fs::remove_dir_all(&first);
    let _ = std::fs::remove_dir_all(&second);
}

#[test]
fn a_picture_requiring_what_this_klipp_does_not_know_is_refused() {
    let (seq, ..) = sequence();
    let dir = scratch("unknown");
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let path = dir.join("project.json");
    let mut json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    json["sequences"][0]["tracks"][0]["clips"][0]["graph_v2"]["requires"] = serde_json::json!(["picture-truth-1", "picture-truth-2"]);
    std::fs::write(&path, serde_json::to_vec_pretty(&json).unwrap()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let err = load_project(&dir).expect_err("an unknown capability is refused");
    assert!(err.contains("unsupported picture capability: picture-truth-2"), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), before, "refusing is not touching");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A Klipp from before `requires` reads a clip's stored picture as a
/// `ClipPicture` that denies unknown fields, so it refuses the mark rather
/// than reading the picture without it. Its rule, on today's type: a field
/// it doesn't know is refused, the picture with it.
#[test]
fn a_reader_from_before_the_mark_refuses_it_as_an_unknown_field() {
    let (_, masked, _) = sequence();
    let mut picture = serde_json::to_value(&masked.picture_v2().unwrap().picture).unwrap();
    picture["requires_v9"] = serde_json::json!(["picture-truth-9"]);
    let err = serde_json::from_value::<klipp_graph_format::lifted::ClipPicture>(picture).expect_err("an unknown field is refused");
    assert!(err.to_string().contains("unknown field"), "{err}");
}

/// M3 step 17a: a grade correction's mix and mask channel are saved in the
/// clip's picture, which then requires `grade-mix-1` as well, read back
/// whole (the renderer's grade carrying them again), and saved again the
/// same. A reader whose capabilities don't include it refuses the picture.
#[test]
fn a_corrections_mix_saves_with_its_capability_and_reopens_the_same() {
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut c = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    let mut g = NodeGraph::default();
    let corrector = g.ensure_corrector();
    c.set_grade(g);
    let p = c.picture_v2().unwrap().picture.clone();
    let group = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).unwrap().id;
    let path = NodePath::new(Root::Clip { clip: c.id.0 }, [group, corrector]);
    c.apply_picture_ops(&[GraphOp::SetMix { path: path.clone(), mix: Some(0.4) }, GraphOp::SetMaskChannel { path, channel: Some("luminance".into()) }]).unwrap();
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips = vec![c.clone()];
    let first = scratch("grade_mix_first");
    save_project(&first, std::slice::from_ref(&seq), &MediaPool::new()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(first.join("project.json")).unwrap()).unwrap();
    let requires = clips(&json)[0]["graph_v2"]["requires"].clone();
    assert_eq!(requires, serde_json::json!(["picture-truth-1", "grade-mix-1"]));
    let names: Vec<String> = serde_json::from_value(requires).unwrap();
    assert_eq!(klipp_graph_format::lifted::unknown_capability(&names, &["picture-truth-1"]), Some("grade-mix-1"), "a reader before it refuses the picture");

    let loaded = load_project(&first).unwrap();
    let reopened = loaded.0[0].tracks[0].clips[0].clone();
    assert_eq!(reopened.picture_v2().unwrap().picture, c.picture_v2().unwrap().picture);
    let run = reopened.grade_to_run();
    let blend = run.nodes.iter().find(|n| n.id == corrector).unwrap().params.correction_blend.expect("the mix, run again");
    assert_eq!((blend.mix, blend.channel), (0.4, klipp_effects::mask_mix::MaskChannel::Luminance));

    let second = scratch("grade_mix_second");
    save_project(&second, &loaded.0, &loaded.1).unwrap();
    assert_eq!(project_text(&second), project_text(&first), "saved again byte for byte");
    let _ = std::fs::remove_dir_all(&first);
    let _ = std::fs::remove_dir_all(&second);
}

/// #2457: a clip id twice in one sequence. Each picture-truth clip is
/// saved and read back whole (its mask and its mix), the later one under
/// an id of its own; the clip linked to it follows it there; and a second
/// save writes the same ids again.
#[test]
fn a_repeated_clip_id_keeps_each_clips_picture() {
    let (mut seq, masked, _) = sequence();
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut again = masked.clone();
    (again.timeline_in, again.timeline_out) = (t(8.0), t(12.0));
    let mut sound = Clip::media(MediaId::new(), t(8.0), t(12.0), t(0.0), t(4.0));
    sound.linked_clip_id = Some(masked.id);
    seq.tracks[0].clips.push(again);
    let audio = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio).expect("an audio track");
    seq.tracks[audio].clips.push(sound);
    let first = scratch("repeated");
    save_project(&first, std::slice::from_ref(&seq), &MediaPool::new()).unwrap();

    let loaded = load_project(&first).unwrap();
    let pictures: Vec<&Clip> = loaded.0[0].tracks[0].clips.iter().filter(|c| c.is_picture_truth()).collect();
    assert_eq!(pictures.len(), 2, "both read back as picture-truth clips");
    for c in &pictures {
        let p = serde_json::to_string(&c.picture_v2().unwrap().picture).unwrap();
        assert!(p.contains("matte_selection") && p.contains("0.5"), "each keeps its mask and its mix: {p}");
    }
    assert_eq!(pictures[0].id, masked.id, "the first keeps its id");
    assert_ne!(pictures[1].id, masked.id, "the later one has its own");
    let linked = loaded.0[0].tracks[audio].clips[0].linked_clip_id;
    assert_eq!(linked, Some(pictures[1].id), "its linked sound follows it");

    let second = scratch("repeated_again");
    save_project(&second, std::slice::from_ref(&seq), &MediaPool::new()).unwrap();
    assert_eq!(project_text(&second), project_text(&first), "the same ids, save after save");
    let _ = std::fs::remove_dir_all(&first);
    let _ = std::fs::remove_dir_all(&second);
}
