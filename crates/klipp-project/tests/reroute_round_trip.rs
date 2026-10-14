//! A clip whose picture routes its chain through a reroute (#1746) comes
//! back from a project as it went in, read by the strict loader: the
//! reroute in the chain, read and read from as it was.

use klipp_core::Timecode;
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;
use klipp_media::MediaPool;
use klipp_project::{load_project_strict, save_project};
use klipp_timeline::{Clip, Sequence};

#[test]
fn a_reroute_in_the_chain_comes_back_as_it_went_in() {
    let dir = std::env::temp_dir().join(format!("klipp_reroute_round_trip_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let tc = |s: f64| Timecode::from_secs_f64(s, 24);
    let mut clip = Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0));
    let picture = clip.picture_v2().unwrap().picture.clone();
    let at = |id| NodePath::new(Root::Clip { clip: clip.id.0 }, [id]);
    let display = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let reroute: klipp_graph_format::Node = serde_json::from_value(serde_json::json!({
        "id": klipp_graph_format::NodeId::new_v4(),
        "type_id": "klipp.legacy.node",
        "label": "",
        "legacy": { "payload": { "kind": "Reroute", "label": "Reroute", "params": {} } },
    }))
    .unwrap();
    clip.apply_picture_ops(&[GraphOp::AddNode { after: at(display), node: Box::new(reroute.clone()) }]).unwrap();
    assert!(clip.is_picture_truth(), "only the picture holds a reroute");
    let clip_id = clip.id;
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips = vec![clip.clone()];

    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project_strict(&dir).unwrap();
    let back = sequences[0].tracks[0].clips.iter().find(|c| c.id == clip_id).expect("the clip");
    let came = &back.picture_v2().unwrap().picture;
    let node = came.graph.nodes.iter().find(|n| n.id == reroute.id).expect("the reroute came back");
    assert_eq!(klipp_graph_lift::structure::kind_of(node), "Reroute");
    assert!(came.graph.edges.iter().any(|e| e.from.0 == display && e.to.0 == reroute.id), "it reads the display step");
    assert!(came.graph.edges.iter().any(|e| e.from.0 == reroute.id), "and is read");
    let _ = std::fs::remove_dir_all(&dir);
}
