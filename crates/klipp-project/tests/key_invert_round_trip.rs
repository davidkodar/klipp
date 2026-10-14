//! A correction's gate taking its key inverted (`key_invert`, an outside
//! node's, #898) comes back from a save and a strict load as it went in,
//! and an inside node beside it comes back without it.

use klipp_media::MediaPool;
use klipp_nodes::NodeKind;
use klipp_project::{load_project_strict, save_project};
use klipp_timeline::{GradeAccess, Sequence, TrackKind};

#[test]
fn an_outside_node_comes_back_inverted() {
    let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
    let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    let mut graph = klipp_nodes::NodeGraph::default();
    let (_, inside) = graph.ensure_masked_secondary(NodeKind::Corrector);
    let outside = graph.add_outside_node(inside).expect("an outside node");
    clip.set_grade(graph);
    let id = clip.id;
    let mut seq = Sequence::new_default();
    let v1 = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
    seq.tracks[v1].clips.push(clip);
    let dir = std::env::temp_dir().join(format!("klipp-key-invert-rt-{}", uuid::Uuid::new_v4()));
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (loaded, ..) = load_project_strict(&dir).unwrap();
    let grade = loaded[0].all_clips().find(|c| c.id == id).expect("the clip").grade().clone();
    let invert = |id| grade.nodes.iter().find(|n| n.id == id).map(|n| n.params.key_invert);
    assert_eq!(invert(outside), Some(Some(true)), "the outside node, inverted");
    assert_eq!(invert(inside), Some(None), "the inside node, as it was");
    assert_eq!(grade.input_of(outside, 1), grade.input_of(inside, 1), "the same key");
    assert_eq!(grade.input_of(outside, 0), Some(inside));
    std::fs::remove_dir_all(dir).unwrap();
}
