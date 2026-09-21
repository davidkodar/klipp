//! What a Blend or a Layer costs where it sits.
//!
//! Both are two-input nodes, and the GPU chain is a single-input
//! ping-pong. So a Blend in the middle of a grade forces the chain to
//! flush — a GPU→CPU readback — run the node on the CPU, and start a
//! new chain afterwards. The node's own arithmetic is a lerp and
//! costs nothing worth measuring; the readback is the bill.
//!
//! Sibling of `secondary_cost.rs`, same resolution and the same
//! caveats: the harness hands the executor a CPU frame, so the Source
//! node really does clone it, and `Output#output` is a flush the
//! viewer's fast path avoids. Read the `extends` / `flushes` / `cpu`
//! counters rather than the absolute milliseconds.
//!
//! Run with `--nocapture`.

use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId, NodePos};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 1214;
const H: u32 = 2160;

fn node(id: NodeId, kind: NodeKind, params: NodeParams) -> Node {
    Node {
        id,
        kind,
        label: format!("{kind:?}"),
        enabled: true,
        params,
        keyframes: Default::default(),
        output_link: None,
        input_links: Vec::new(),
    }
}

fn primary() -> NodeParams {
    NodeParams {
        temperature: Some(-15.0),
        tint: Some(3.0),
        shadows: Some(0.006),
        contrast: Some(0.03),
        highlights: Some(-0.015),
        clarity: Some(0.05),
        ..Default::default()
    }
}

fn graph(nodes: Vec<Node>, edges: Vec<Edge>, output: NodeId) -> NodeGraph {
    let node_positions = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id, NodePos { x: i as f32 * 200.0, y: 0.0 }))
        .collect();
    NodeGraph {
        output_node_id: output,
        nodes,
        edges,
        node_positions,
        working_space_override: None,
        frames: Vec::new(),
        sticky_notes: Vec::new(),
    }
}

/// `Source → Corrector → Output`: the baseline, one chain, no readback
/// until the end.
fn primary_only() -> NodeGraph {
    let (src, corr, out) = (NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4());
    graph(
        vec![
            node(src, NodeKind::Source, Default::default()),
            node(corr, NodeKind::Corrector, primary()),
            node(out, NodeKind::Output, Default::default()),
        ],
        vec![
            Edge { from_node: src, from_port: 0, to_node: corr, to_port: 0 },
            Edge { from_node: corr, from_port: 0, to_node: out, to_port: 0 },
        ],
        out,
    )
}

/// The shape a colourist builds to dial a grade back: correct the
/// shot, then mix the corrected picture with the untouched one.
///
///   Source ─┬─► Corrector ─► Blend.0
///           └──────────────► Blend.1
fn blend_back_with_source(kind: NodeKind, params: NodeParams) -> NodeGraph {
    let (src, corr, mix, out) =
        (NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4());
    graph(
        vec![
            node(src, NodeKind::Source, Default::default()),
            node(corr, NodeKind::Corrector, primary()),
            node(mix, kind, params),
            node(out, NodeKind::Output, Default::default()),
        ],
        vec![
            Edge { from_node: src, from_port: 0, to_node: corr, to_port: 0 },
            Edge { from_node: corr, from_port: 0, to_node: mix, to_port: 0 },
            Edge { from_node: src, from_port: 0, to_node: mix, to_port: 1 },
            Edge { from_node: mix, from_port: 0, to_node: out, to_port: 0 },
        ],
        out,
    )
}

/// Two graded branches recombined — the general case, where neither
/// input is the bare source.
fn blend_two_branches() -> NodeGraph {
    let (src, warm, cool, mix, out) = (
        NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4(),
        NodeId::new_v4(), NodeId::new_v4(),
    );
    graph(
        vec![
            node(src, NodeKind::Source, Default::default()),
            node(warm, NodeKind::Corrector, primary()),
            node(cool, NodeKind::Corrector, NodeParams { temperature: Some(-45.0), ..Default::default() }),
            node(mix, NodeKind::Blend, NodeParams { mix: Some(0.5), ..Default::default() }),
            node(out, NodeKind::Output, Default::default()),
        ],
        vec![
            Edge { from_node: src, from_port: 0, to_node: warm, to_port: 0 },
            Edge { from_node: src, from_port: 0, to_node: cool, to_port: 0 },
            Edge { from_node: warm, from_port: 0, to_node: mix, to_port: 0 },
            Edge { from_node: cool, from_port: 0, to_node: mix, to_port: 1 },
            Edge { from_node: mix, from_port: 0, to_node: out, to_port: 0 },
        ],
        out,
    )
}

fn frame() -> Vec<f32> {
    let mut px = vec![0.0f32; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let t = x as f32 / W as f32;
            let v = 0.15 + 0.7 * (y as f32 / H as f32);
            px[i] = v * (0.6 + 0.4 * t);
            px[i + 1] = v * (0.5 + 0.2 * (1.0 - t));
            px[i + 2] = v * (0.4 + 0.5 * (1.0 - t));
            px[i + 3] = 1.0;
        }
    }
    px
}

#[test]
fn where_a_composites_frame_time_goes() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_target(false)
        .without_time()
        .try_init();

    let src = frame();
    let mut exec = GraphExecutor::new();
    for (name, g) in [
        ("primary only", primary_only()),
        (
            "blend back with source",
            blend_back_with_source(NodeKind::Blend, NodeParams { mix: Some(0.5), ..Default::default() }),
        ),
        ("blend two graded branches", blend_two_branches()),
        (
            "layer over source",
            blend_back_with_source(NodeKind::Layer, Default::default()),
        ),
    ] {
        let _ = exec.execute_linear(&src, &g, W, H, 0.0, 0, None, None);
        let mut best = f64::INFINITY;
        for _ in 0..3 {
            let t0 = std::time::Instant::now();
            let r = exec.execute_linear(&src, &g, W, H, 0.0, 0, None, None);
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            assert!(r.is_ok(), "{name} failed to execute");
            best = best.min(ms);
        }
        let s = exec.last_run_chain_stats();
        eprintln!(
            "=== {name}: best of 3 = {best:.1} ms  (extends={} flushes={} cpu={})",
            s.extends, s.flushes, s.cpu_nodes,
        );
    }
}
