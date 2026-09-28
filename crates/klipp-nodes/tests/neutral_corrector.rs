//! A Corrector at its defaults in front of the rest of a grade.
//!
//! It is what a clip's graph holds before anything is graded, and what
//! a grade reset leaves: a neutral Corrector, then a look. It changes no
//! pixel, and it used to cost the whole frame a trip through the CPU. It
//! was neither bypassed (the Corrector was missing from the kinds a
//! default-param node is bypassed for) nor chainable (no stage active),
//! so with the frame already on the GPU the run failed with
//! `CpuSourceRequired` and the engine read the frame back to run it on
//! the CPU: about 105 ms a frame at 2160x3840, 8 fps in playback.

use std::sync::Arc;

use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId, NodePos};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 64;
const H: u32 = 48;

fn node(id: NodeId, kind: NodeKind, params: NodeParams) -> Node {
    Node {
        id,
        kind,
        label: format!("{kind:?}"),
        enabled: true,
        params,
        keyframes: Default::default(),
        output_link: None,
        input_links: vec![],
    }
}

/// Source, then `middle` in a line, then Output.
fn chain(middle: Vec<(NodeKind, NodeParams)>) -> NodeGraph {
    let mut nodes = vec![node(NodeId::new_v4(), NodeKind::Source, NodeParams::default())];
    for (kind, params) in middle {
        nodes.push(node(NodeId::new_v4(), kind, params));
    }
    nodes.push(node(NodeId::new_v4(), NodeKind::Output, NodeParams::default()));
    let edges = nodes
        .windows(2)
        .map(|w| Edge { from_node: w[0].id, from_port: 0, to_node: w[1].id, to_port: 0 })
        .collect();
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 200.0, y: 0.0 })).collect();
    NodeGraph {
        output_node_id: nodes.last().unwrap().id,
        nodes,
        edges,
        node_positions,
        working_space_override: None,
        frames: Vec::new(),
        sticky_notes: Vec::new(),
    }
}

fn brighter() -> (NodeKind, NodeParams) {
    (NodeKind::Exposure, NodeParams { exposure: Some(0.5), ..Default::default() })
}

fn neutral_corrector() -> (NodeKind, NodeParams) {
    (NodeKind::Corrector, NodeParams::default())
}

fn frame_srgb8() -> Vec<u8> {
    (0..W * H).flat_map(|i| [(i % 251) as u8, (i % 127) as u8 * 2, 90, 255]).collect()
}

fn frame_linear() -> Vec<f32> {
    (0..W * H).flat_map(|i| [(i % 97) as f32 / 97.0, (i % 53) as f32 / 53.0, 0.3, 1.0]).collect()
}

#[test]
fn a_neutral_corrector_keeps_a_gpu_frame_on_the_gpu() {
    let mut exec = GraphExecutor::new();
    let g = chain(vec![neutral_corrector(), brighter()]);
    // The frame is on the GPU already, as a decoded frame is in playback:
    // the run gets no CPU pixels to fall back to. (The GPU comes up on
    // first use, so the seed is what says whether there is one.)
    if !exec.prime_chain_from_srgb8(&Arc::new(frame_srgb8()), W, H) {
        eprintln!("skipping: no GPU");
        return;
    }
    let run = exec.execute_linear(&[], &g, W, H, 0.0, 0, None, None);
    assert!(run.is_ok(), "the run needed the frame on the CPU: {:?}", run.err());
    assert_eq!(exec.last_run_chain_stats().cpu_nodes, 0, "no node ran on the CPU");
}

#[test]
fn a_neutral_corrector_changes_no_pixel() {
    let mut exec = GraphExecutor::new();
    let src = frame_linear();
    let (with, _) = exec.execute_linear(&src, &chain(vec![neutral_corrector(), brighter()]), W, H, 0.0, 0, None, None).unwrap();
    let (without, _) = exec.execute_linear(&src, &chain(vec![brighter()]), W, H, 0.0, 0, None, None).unwrap();
    assert_eq!(with.len(), without.len());
    let worst = with.iter().zip(&without).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(worst < 1e-6, "a neutral Corrector moved a value by {worst}");
}

