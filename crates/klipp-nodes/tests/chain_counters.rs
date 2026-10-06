//! `ChainStats::submits` and `uploads` for known chains: a GPU chain of
//! any length is one segment, so one command buffer submitted and one
//! frame uploaded, every run; nothing to run is neither, and the CPU
//! executor never touches the GPU.
//!
//! A missing GPU is reported as unverified; with `KLIPP_TEST_REQUIRE_GPU=1`
//! it fails instead.

use klipp_nodes::executor::ChainStats;
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId, NodePos};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 64;
const H: u32 = 36;

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() }
}

fn exposure() -> Node {
    node(NodeKind::Exposure, NodeParams { exposure: Some(-0.5), ..Default::default() })
}

fn corrector() -> Node {
    node(NodeKind::Corrector, NodeParams { contrast: Some(0.1), ..Default::default() })
}

/// `Source → mid… → Output`.
fn chain(mid: Vec<Node>) -> NodeGraph {
    let mut nodes = vec![node(NodeKind::Source, Default::default())];
    nodes.extend(mid);
    nodes.push(node(NodeKind::Output, Default::default()));
    let edges = nodes.windows(2).map(|w| Edge { from_node: w[0].id, from_port: 0, to_node: w[1].id, to_port: 0 }).collect();
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
    NodeGraph { output_node_id: nodes.last().unwrap().id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

/// Each of two runs' stats: the second shows a count is the run's own,
/// not the executor's total.
fn runs(g: &NodeGraph, gpu: bool) -> [ChainStats; 2] {
    let frame = vec![0.4f32; (W * H * 4) as usize];
    let mut e = if gpu { GraphExecutor::new() } else { GraphExecutor::new_cpu_only() };
    [0, 1].map(|_| {
        e.execute_linear(&frame, g, W, H, 0.0, 0, None, None).expect("graph runs");
        e.last_run_chain_stats()
    })
}

fn counts(s: &ChainStats) -> (u32, u32) {
    (s.submits, s.uploads)
}

#[test]
fn a_gpu_chain_submits_once_and_uploads_its_input_once() {
    let one = runs(&chain(vec![exposure()]), true);
    if one[0].flushes == 0 {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required but no GPU chain ran: {:?}", one[0]);
        eprintln!("UNVERIFIED: no GPU chain ran ({:?})", one[0]);
        return;
    }
    for s in &one {
        assert_eq!(counts(s), (1, 1), "one node: {s:?}");
    }
    for s in &runs(&chain(vec![exposure(), corrector(), exposure()]), true) {
        assert_eq!(s.extends, 2, "three nodes, one segment: {s:?}");
        assert_eq!(counts(s), (1, 1), "three nodes batch into one submit: {s:?}");
    }
    for s in &runs(&chain(vec![]), true) {
        assert_eq!(counts(s), (0, 0), "nothing to run: {s:?}");
    }
}

#[test]
fn the_cpu_executor_neither_submits_nor_uploads() {
    for s in &runs(&chain(vec![exposure(), corrector()]), false) {
        assert_eq!(counts(s), (0, 0), "{s:?}");
    }
}
