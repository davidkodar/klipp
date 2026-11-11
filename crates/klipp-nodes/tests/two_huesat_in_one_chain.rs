//! Two Hue/Sat curve nodes, or two RGB curve nodes, in one graph, each with
//! its own curves, run on
//! the GPU as one chain. A node's curves go up as a table by a queue write,
//! which lands before the chain's encoder runs whatever was recorded
//! ahead of it, so two passes sharing one table buffer would both read the
//! one written last. Each must read its own: the GPU's picture is the CPU's.

use klipp_core::{HueSatCurves, WorkingColorspace};
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId, NodePos};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 128;
const H: u32 = 64;

fn node(id: NodeId, kind: NodeKind, params: NodeParams) -> Node {
    Node { id, kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() }
}

fn graph(chain: Vec<NodeParams>) -> NodeGraph {
    graph_of(NodeKind::HueSatCurves, chain)
}

fn graph_of(kind: NodeKind, chain: Vec<NodeParams>) -> NodeGraph {
    let mut nodes = vec![node(NodeId::new_v4(), NodeKind::Source, Default::default())];
    nodes.extend(chain.into_iter().map(|p| node(NodeId::new_v4(), kind, p)));
    nodes.push(node(NodeId::new_v4(), NodeKind::Output, Default::default()));
    let edges = nodes.windows(2).map(|w| Edge { from_node: w[0].id, from_port: 0, to_node: w[1].id, to_port: 0 }).collect();
    let node_positions = nodes.iter().enumerate().map(|(i, m)| (m.id, NodePos { x: i as f32 * 200.0, y: 0.0 })).collect();
    NodeGraph { output_node_id: nodes.last().unwrap().id, nodes, edges, node_positions, working_space_override: None, frames: Vec::new(), sticky_notes: Vec::new() }
}

/// Every hue across, light to dark down: something for each curve to move.
fn frame() -> Vec<f32> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let (h, l) = (x as f32 / W as f32 * 6.0, 0.15 + 0.7 * y as f32 / (H - 1) as f32);
            let f = |o: f32| ((h + o) % 6.0 - 3.0).abs().clamp(1.0, 2.0) - 1.0;
            v.extend_from_slice(&[l * (0.3 + 0.7 * (1.0 - f(0.0))), l * (0.3 + 0.7 * (1.0 - f(4.0))), l * (0.3 + 0.7 * (1.0 - f(2.0))), 1.0]);
        }
    }
    v
}

fn run(g: &NodeGraph, gpu: bool) -> (Vec<f32>, klipp_nodes::executor::ChainStats) {
    let mut exec = if gpu { GraphExecutor::new() } else { GraphExecutor::new_cpu_only() };
    exec.set_working_colorspace(WorkingColorspace::Rec709Linear);
    let out = exec.execute_linear(&frame(), g, W, H, 0.0, 0, None, None).expect("graph runs").0;
    (out, exec.last_run_chain_stats())
}

fn worst_levels(a: &[f32], b: &[f32]) -> f32 {
    let enc = |v: f32| klipp_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0));
    a.chunks_exact(4).zip(b.chunks_exact(4)).flat_map(|(p, q)| (0..3).map(move |i| (enc(p[i]) - enc(q[i])).abs())).fold(0.0f32, f32::max) * 255.0
}

#[test]
fn two_huesat_nodes_in_one_chain_each_read_their_own_curves() {
    // One takes the colour out, the other turns the hues.
    let a = NodeParams { huesat: Some(HueSatCurves { hue_vs_sat: vec![[0.0, 0.15], [1.0, 0.15]], ..Default::default() }), ..Default::default() };
    let b = NodeParams { huesat: Some(HueSatCurves { hue_vs_hue: vec![[0.0, 0.7], [1.0, 0.7]], ..Default::default() }), ..Default::default() };
    let both = graph(vec![a, b.clone()]);
    let (cpu, _) = run(&both, false);
    let (gpu, stats) = run(&both, true);
    if stats.extends == 0 && stats.flushes == 0 {
        assert!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref() != Ok("1"), "no usable GPU");
        eprintln!("skipping: no usable GPU");
        return;
    }
    let (only_b, _) = run(&graph(vec![b.clone(), b]), false);
    assert!(worst_levels(&cpu, &only_b) > 4.0, "the two curves give the same picture: the test can't see a shared table");
    let w = worst_levels(&cpu, &gpu);
    assert!(w < 1.0, "the GPU's two Hue/Sat nodes differ from the CPU's by {w:.2} display levels: one read the other's curves");
}

/// The same for two RGB curve nodes.
#[test]
fn two_rgb_curve_nodes_in_one_chain_each_read_their_own_curves() {
    let curves = |master: Vec<[f32; 2]>, red: Vec<[f32; 2]>| NodeParams { curves: Some(klipp_core::CurveSet { master, red, ..Default::default() }), ..Default::default() };
    let a = curves(vec![[0.0, 0.0], [0.5, 0.75], [1.0, 1.0]], Vec::new());
    let b = curves(Vec::new(), vec![[0.0, 0.0], [0.5, 0.25], [1.0, 1.0]]);
    let both = graph_of(NodeKind::RGBCurves, vec![a, b.clone()]);
    let (cpu, _) = run(&both, false);
    let (gpu, stats) = run(&both, true);
    if stats.extends == 0 && stats.flushes == 0 {
        assert!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref() != Ok("1"), "no usable GPU");
        return;
    }
    let (only_b, _) = run(&graph_of(NodeKind::RGBCurves, vec![b.clone(), b]), false);
    assert!(worst_levels(&cpu, &only_b) > 4.0, "the two curves give the same picture");
    let w = worst_levels(&cpu, &gpu);
    assert!(w < 1.0, "the GPU's two RGB curve nodes differ from the CPU's by {w:.2} display levels: one read the other's curves");
}
