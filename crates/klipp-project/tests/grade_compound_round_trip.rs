//! A clip whose picture holds a grade in its chain (#1746) comes back from
//! a project as it went in, read by the strict loader: the compound after
//! the display step, its body's Corrector and its settings.

use klipp_core::Timecode;
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;
use klipp_media::MediaPool;
use klipp_project::{load_project_strict, save_project};
use klipp_timeline::{Clip, Sequence};

#[test]
fn a_grade_compound_comes_back_as_it_went_in() {
    let dir = std::env::temp_dir().join(format!("klipp_grade_compound_round_trip_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let tc = |s: f64| Timecode::from_secs_f64(s, 24);
    let mut clip = Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0));
    let picture = clip.picture_v2().unwrap().picture.clone();
    let at = |ids: &[klipp_graph_format::NodeId]| NodePath::new(Root::Clip { clip: clip.id.0 }, ids.iter().copied());
    let display = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let mut grade = klipp_nodes::NodeGraph::default();
    let corrector = grade.ensure_corrector();
    grade.nodes.iter_mut().find(|n| n.id == corrector).unwrap().params.exposure = Some(0.6);
    let compound = klipp_graph_lift::grade_compound::compound_of(&grade, klipp_graph_format::NodeId::new_v4()).unwrap();
    clip.apply_picture_ops(&[GraphOp::AddNode { after: at(&[display]), node: Box::new(compound.clone()) }]).unwrap();
    assert!(clip.is_picture_truth(), "only the picture holds a compound");
    let clip_id = clip.id;
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips = vec![clip.clone()];

    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project_strict(&dir).unwrap();
    let back = sequences[0].tracks[0].clips.iter().find(|c| c.id == clip_id).expect("the clip");
    let came = &back.picture_v2().unwrap().picture;
    let node = came.graph.nodes.iter().find(|n| n.id == compound.id).expect("the compound came back");
    assert!(klipp_graph_lift::grade_compound::is_compound(node));
    assert!(came.graph.edges.iter().any(|e| e.from.0 == display && e.to.0 == compound.id), "after the display step");
    let run = klipp_timeline::lowered_grade(&node.group.as_ref().unwrap().graph).unwrap();
    let c = run.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).expect("its Corrector");
    assert_eq!(c.params.exposure, Some(0.6));
    let _ = std::fs::remove_dir_all(&dir);
}
