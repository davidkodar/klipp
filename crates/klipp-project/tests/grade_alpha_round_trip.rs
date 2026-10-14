//! A matte wired into a grade's Output alpha (#898) comes back from a save
//! and a strict load still wired there, not into the picture input.

use klipp_core::{Mask, MaskShape, MaskStack};
use klipp_media::MediaPool;
use klipp_nodes::{Edge, NodeKind};
use klipp_project::{load_project_strict, save_project};
use klipp_timeline::{GradeAccess, Sequence, TrackKind};

#[test]
fn a_matte_into_the_grades_alpha_comes_back_wired_there() {
    let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
    let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    let mut graph = klipp_nodes::NodeGraph::default();
    graph.ensure_corrector();
    let source = graph.nodes.iter().find(|n| n.kind == NodeKind::Source).expect("the Source").id;
    let mask = klipp_nodes::NodeId::new_v4();
    graph.nodes.push(klipp_nodes::Node {
        id: mask,
        kind: NodeKind::Mask,
        label: "Mask".into(),
        enabled: true,
        params: klipp_nodes::NodeParams {
            masks: Some(MaskStack { masks: vec![Mask { shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.3 }, ..Mask::default() }] }),
            ..Default::default()
        },
        keyframes: Default::default(),
        output_link: None,
        input_links: Vec::new(),
    });
    graph.edges.push(Edge { from_node: source, from_port: 0, to_node: mask, to_port: 0 });
    graph.edges.push(Edge { from_node: mask, from_port: 0, to_node: graph.output_node_id, to_port: 1 });
    clip.set_grade(graph);
    let id = clip.id;
    let mut seq = Sequence::new_default();
    let v1 = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
    seq.tracks[v1].clips.push(clip);
    let dir = std::env::temp_dir().join(format!("klipp-grade-alpha-rt-{}", uuid::Uuid::new_v4()));
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (loaded, ..) = load_project_strict(&dir).unwrap();
    let clip = loaded[0].all_clips().find(|c| c.id == id).expect("the clip");
    let grade = clip.grade();
    let into_output: Vec<_> = grade.edges.iter().filter(|e| e.to_node == grade.output_node_id).map(|e| (e.from_node, e.to_port)).collect();
    assert!(into_output.contains(&(mask, 1)), "the matte feeds the Output's alpha: {into_output:?}");
    assert_eq!(into_output.len(), 2, "and the picture still feeds its input: {into_output:?}");
    std::fs::remove_dir_all(dir).unwrap();
}
