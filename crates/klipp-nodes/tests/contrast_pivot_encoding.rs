//! A Contrast node and a Corrector pivot contrast at the same place in
//! every grade encoding (#1039). The Contrast node encoded its pivot in
//! BT.1886 whatever the grade's encoding was, so under ACEScct the same
//! pivot meant another brightness than it did on a Corrector.

use klipp_core::{GradeEncoding, WorkingColorspace};
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 16;
const H: u32 = 4;

/// A grey ramp across.
fn frame() -> Vec<u8> {
    (0..W * H).flat_map(|i| {
        let v = ((i % W) * 255 / (W - 1)) as u8;
        [v, v, v, 255]
    }).collect()
}

/// Source → one node of `kind` with `params` → Output.
fn graph(kind: NodeKind, params: NodeParams) -> NodeGraph {
    let node = |kind, params| Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() };
    let nodes = vec![node(NodeKind::Source, NodeParams::default()), node(kind, params), node(NodeKind::Output, NodeParams::default())];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    let output_node_id = nodes[2].id;
    NodeGraph { nodes, edges, output_node_id, ..Default::default() }
}

#[test]
fn a_contrast_node_pivots_where_a_corrector_does_in_every_encoding() {
    let params = || NodeParams { contrast: Some(0.6), contrast_pivot: Some(0.05), ..Default::default() };
    for (ws, enc) in [
        (WorkingColorspace::Rec709Linear, GradeEncoding::Bt1886),
        (WorkingColorspace::AcesCG, GradeEncoding::AcesCct),
        (WorkingColorspace::AcesCG, GradeEncoding::AcesCc),
    ] {
        for (path, mut ex) in [("cpu", GraphExecutor::new_cpu_only()), ("default", GraphExecutor::new())] {
            ex.set_working_colorspace(ws);
            ex.set_grade_encoding(enc);
            let contrast = ex.execute(&frame(), &graph(NodeKind::Contrast, params()), W, H, 0.0, 0, None, None).unwrap().0;
            let corrector = ex.execute(&frame(), &graph(NodeKind::Corrector, params()), W, H, 0.0, 0, None, None).unwrap().0;
            let worst = contrast.iter().zip(&corrector).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            assert!(worst <= 1, "{enc:?} ({path}): the Contrast node and the Corrector differ by up to {worst} levels");
            assert_ne!(contrast, frame(), "{enc:?} ({path}): the contrast did something");
        }
    }
}
