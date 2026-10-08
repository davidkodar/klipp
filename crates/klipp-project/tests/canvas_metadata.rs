//! Root-canvas labels and placement survive the project format boundary.
use klipp_core::{MediaId, RationalTime};
use klipp_effects::BuiltinEffect;
use klipp_graph_format::{op::GraphOp, path::{NodePath, Root}};
use klipp_timeline::{Clip, Sequence};

#[test]
fn canvas_metadata_survives_saving_and_reopening() {
    let t = |s| RationalTime::from_secs_f64(s, 25);
    let mut c = Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
    c.push_effect(BuiltinEffect::GaussianBlur { radius: 2.0 });
    let graph = c.picture_v2().unwrap();
    let node = graph.picture.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::types::LEGACY_EFFECT).unwrap().id;
    c.apply_picture_ops(&[GraphOp::SetNodeMeta { path: NodePath::new(Root::Clip { clip: c.id.0 }, [node]), label: Some("Soft background".into()), position: Some(serde_json::json!({"x":420.0,"y":160.0})) }]).unwrap();
    let expected = c.picture_v2().unwrap();
    let mut sequence = Sequence::new_default();
    sequence.tracks[0].clips.push(c);
    let dir = std::env::temp_dir().join(format!("klipp-canvas-metadata-{}", uuid::Uuid::new_v4()));
    klipp_project::save_project(&dir, &[sequence], &klipp_media::MediaPool::new()).unwrap();
    let loaded = klipp_project::load_project(&dir).unwrap();
    let reopened = loaded.0[0].tracks[0].clips[0].picture_v2().unwrap();
    assert_eq!(reopened.picture, expected.picture);
    std::fs::remove_dir_all(dir).unwrap();
}
