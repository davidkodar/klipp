//! A clip whose picture lays another media item over by a Merge (#1746)
//! comes back from a project as it went in, read by the strict loader: the
//! media source beside the chain with its item and offset, and the wire
//! into the merge's `fg`. The item is named by its id, so a relinked file
//! is read without picking it again.

use klipp_core::Timecode;
use klipp_effects::{BlendMode, BuiltinEffect};
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;
use klipp_media::MediaPool;
use klipp_project::{load_project_strict, save_project};
use klipp_timeline::{Clip, Sequence};

fn effect_node(e: &BuiltinEffect) -> klipp_graph_format::Node {
    let payload = serde_json::to_value(e).unwrap();
    serde_json::from_value(serde_json::json!({
        "id": klipp_graph_format::NodeId::new_v4(),
        "type_id": klipp_graph_lift::names::effect_node_type(payload["type"].as_str().unwrap()),
        "legacy": { "payload": payload },
    }))
    .unwrap()
}

#[test]
fn a_merged_media_source_comes_back_as_it_went_in() {
    let dir = std::env::temp_dir().join(format!("klipp_media_source_round_trip_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let tc = |s: f64| Timecode::from_secs_f64(s, 24);
    let mut clip = Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0));
    let picture = clip.picture_v2().unwrap().picture.clone();
    let at = |id| NodePath::new(Root::Clip { clip: clip.id.0 }, [id]);
    let display = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let source = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
    let merge = effect_node(&BuiltinEffect::Merge { blend_mode: BlendMode::Screen, opacity: 0.6 });
    let item = klipp_core::MediaId::new();
    let source_node = effect_node(&BuiltinEffect::MediaSource { media: item.0.to_string(), offset: -1.25 });
    clip.apply_picture_ops(&[
        GraphOp::AddNode { after: at(display), node: Box::new(merge.clone()) },
        GraphOp::AddNodeBeside { at: at(source), node: Box::new(source_node.clone()) },
        GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(source_node.id), to_port: "in".into() },
        GraphOp::Connect { from: at(source_node.id), from_port: "out".into(), to: at(merge.id), to_port: "fg".into() },
    ])
    .unwrap();
    assert!(clip.is_picture_truth());
    let clip_id = clip.id;
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips = vec![clip.clone()];

    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project_strict(&dir).unwrap();
    let back = sequences[0].tracks[0].clips.iter().find(|c| c.id == clip_id).expect("the clip");
    assert!(back.is_picture_truth());
    let (went, came) = (clip.picture_v2().unwrap(), back.picture_v2().unwrap());
    for id in [merge.id, source_node.id] {
        let node = |p: &klipp_graph_format::lifted::ClipPicture| p.graph.nodes.iter().find(|n| n.id == id).cloned().unwrap_or_else(|| panic!("{id} came back"));
        assert_eq!(klipp_graph_lift::effect_params::effect_of(&node(&came.picture)).unwrap(), klipp_graph_lift::effect_params::effect_of(&node(&went.picture)).unwrap());
    }
    assert!(came.picture.graph.edges.iter().any(|e| e.from.0 == source_node.id && e.to == (merge.id, "fg".into())), "the wire into the merge's picture");
    assert!(came.picture.graph.edges.iter().any(|e| e.from.0 == source && e.to.0 == source_node.id), "the media source stands beside the chain");
    let _ = std::fs::remove_dir_all(&dir);
}
