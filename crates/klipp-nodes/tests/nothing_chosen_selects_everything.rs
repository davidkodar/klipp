//! A matte producer with nothing chosen selects everything (#1039): a
//! correction it gates grades the whole frame, as if there were no key.
//! The Qualifier with no settings and a LumaKey that is off did; a
//! DifferenceKey that is off or has no plate, and an ExternalMatte with
//! no image or a missing one, selected nothing.

use klipp_core::color_grade::{DifferenceKeySettings, LumaKeySettings};
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 16;
const H: u32 = 4;

/// Strong red everywhere.
fn frame() -> Vec<u8> {
    [220u8, 30, 30, 255].repeat((W * H) as usize)
}

/// Source → a Corrector desaturating fully → Output, with no matte.
fn ungated() -> NodeGraph {
    let node = |kind, params| Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() };
    let src = node(NodeKind::Source, NodeParams::default());
    let corr = node(NodeKind::Corrector, NodeParams { saturation: Some(0.0), ..Default::default() });
    let out = node(NodeKind::Output, NodeParams::default());
    let edges = vec![
        Edge { from_node: src.id, from_port: 0, to_node: corr.id, to_port: 0 },
        Edge { from_node: corr.id, from_port: 0, to_node: out.id, to_port: 0 },
    ];
    let output_node_id = out.id;
    NodeGraph { nodes: vec![src, corr, out], edges, output_node_id, ..Default::default() }
}

/// Source → a Corrector desaturating fully, gated on its port 1 by a
/// matte node of `kind` with `params` → Output.
fn gated(kind: NodeKind, params: NodeParams) -> NodeGraph {
    let node = |kind, params| Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() };
    let src = node(NodeKind::Source, NodeParams::default());
    let corr = node(NodeKind::Corrector, NodeParams { saturation: Some(0.0), ..Default::default() });
    let matte = node(kind, params);
    let out = node(NodeKind::Output, NodeParams::default());
    let mut edges = vec![
        Edge { from_node: src.id, from_port: 0, to_node: corr.id, to_port: 0 },
        Edge { from_node: matte.id, from_port: 0, to_node: corr.id, to_port: 1 },
        Edge { from_node: corr.id, from_port: 0, to_node: out.id, to_port: 0 },
    ];
    if kind != NodeKind::ExternalMatte {
        edges.push(Edge { from_node: src.id, from_port: 0, to_node: matte.id, to_port: 0 });
    }
    let output_node_id = out.id;
    NodeGraph { nodes: vec![src, corr, matte, out], edges, output_node_id, ..Default::default() }
}

#[test]
fn every_matte_producer_with_nothing_chosen_selects_the_whole_frame() {
    let missing = std::env::temp_dir().join("klipp-1039-no-such-image.png");
    let cases = [
        ("qualifier, no settings", NodeKind::Qualifier, NodeParams::default()),
        ("luma key, off", NodeKind::LumaKey, NodeParams { luma_key: Some(LumaKeySettings { enabled: false, ..Default::default() }), ..Default::default() }),
        ("mask, no shapes", NodeKind::Mask, NodeParams::default()),
        ("difference key, unset", NodeKind::DifferenceKey, NodeParams::default()),
        ("difference key, off", NodeKind::DifferenceKey, NodeParams { difference_key: Some(DifferenceKeySettings { enabled: false, plate_path: Some(missing.clone()), ..Default::default() }), ..Default::default() }),
        ("difference key, no plate", NodeKind::DifferenceKey, NodeParams { difference_key: Some(DifferenceKeySettings { enabled: true, ..Default::default() }), ..Default::default() }),
        ("difference key, a missing plate", NodeKind::DifferenceKey, NodeParams { difference_key: Some(DifferenceKeySettings { enabled: true, plate_path: Some(missing.clone()), ..Default::default() }), ..Default::default() }),
        ("external matte, no image", NodeKind::ExternalMatte, NodeParams::default()),
        ("external matte, a missing image", NodeKind::ExternalMatte, NodeParams { matte_image_path: Some(missing.clone()), ..Default::default() }),
    ];
    for (what, kind, params) in cases {
        for (path, mut ex) in [("cpu", GraphExecutor::new_cpu_only()), ("default", GraphExecutor::new())] {
            let out = ex.execute(&frame(), &gated(kind, params.clone()), W, H, 0.0, 0, None, None).unwrap().0;
            // As the same correction with no matte at all: graded grey,
            // opaque. A matte of 0 would leave the red, one that cut the
            // alpha would leave it transparent.
            let plain = ex.execute(&frame(), &ungated(), W, H, 0.0, 0, None, None).unwrap().0;
            assert!(plain.chunks_exact(4).all(|p| p[0].abs_diff(p[1]) <= 2 && p[0] < 200), "{path}: the ungraded check is not grey");
            for (px, want) in out.chunks_exact(4).zip(plain.chunks_exact(4)) {
                assert!(px.iter().zip(want).all(|(a, b)| a.abs_diff(*b) <= 1), "{what} ({path}): {px:?}, not the ungated correction's {want:?}");
                assert_eq!(px[3], 255, "{what} ({path}): the alpha stays whole");
            }
        }
    }
}
