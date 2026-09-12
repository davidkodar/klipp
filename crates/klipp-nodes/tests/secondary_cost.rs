//! Where a secondary's frame time goes.
//!
//! Reproduces the graph a qualifier-driven secondary actually builds —
//! a primary Corrector fanning out to a Qualifier, a second Corrector
//! and an ApplyMatte — at the resolution the viewer runs it, and prints
//! the executor's own per-node breakdown (`PERF graph_exec`, debug).
//!
//! Run it with `--nocapture` and read the breakdown; it is a measurement
//! harness, not an assertion of any particular speed.

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

fn wall_window() -> klipp_core::QualifierSettings {
    klipp_core::QualifierSettings {
        enabled: true,
        hue_center: 27.0,
        hue_width: 20.0,
        hue_soft: 10.0,
        sat_low: 0.2,
        sat_high: 0.5,
        sat_soft: 0.05,
        lum_low: 0.1,
        lum_high: 0.85,
        lum_soft: 0.04,
        ..Default::default()
    }
}

/// `Source → Corrector` and nothing else: the baseline every other
/// shape is measured against.
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

/// The primary with a second Corrector behind it — still one straight
/// line, so the GPU chain should carry both without a readback.
fn two_correctors() -> NodeGraph {
    let (src, corr, cool, out) =
        (NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4());
    graph(
        vec![
            node(src, NodeKind::Source, Default::default()),
            node(corr, NodeKind::Corrector, primary()),
            node(cool, NodeKind::Corrector, NodeParams { temperature: Some(-45.0), ..Default::default() }),
            node(out, NodeKind::Output, Default::default()),
        ],
        vec![
            Edge { from_node: src, from_port: 0, to_node: corr, to_port: 0 },
            Edge { from_node: corr, from_port: 0, to_node: cool, to_port: 0 },
            Edge { from_node: cool, from_port: 0, to_node: out, to_port: 0 },
        ],
        out,
    )
}

/// The primary fanning out to a Qualifier, with the matte going nowhere
/// — isolates what the fan-out and the qualifier dispatch cost, without
/// the blend.
fn primary_and_qualifier() -> NodeGraph {
    let (src, corr, qual, out) =
        (NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4());
    graph(
        vec![
            node(src, NodeKind::Source, Default::default()),
            node(corr, NodeKind::Corrector, primary()),
            node(qual, NodeKind::Qualifier, NodeParams { qualifier: Some(wall_window()), ..Default::default() }),
            node(out, NodeKind::Output, Default::default()),
        ],
        vec![
            Edge { from_node: src, from_port: 0, to_node: corr, to_port: 0 },
            Edge { from_node: corr, from_port: 0, to_node: qual, to_port: 0 },
            Edge { from_node: corr, from_port: 0, to_node: out, to_port: 0 },
        ],
        out,
    )
}

/// The whole secondary, as the grade built it.
fn full_secondary() -> NodeGraph {
    let (src, corr, qual, cool, apply, out) = (
        NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4(),
        NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4(),
    );
    graph(
        vec![
            node(src, NodeKind::Source, Default::default()),
            node(corr, NodeKind::Corrector, primary()),
            node(qual, NodeKind::Qualifier, NodeParams { qualifier: Some(wall_window()), ..Default::default() }),
            node(cool, NodeKind::Corrector, NodeParams { temperature: Some(-45.0), ..Default::default() }),
            node(apply, NodeKind::ApplyMatte, Default::default()),
            node(out, NodeKind::Output, Default::default()),
        ],
        vec![
            Edge { from_node: src, from_port: 0, to_node: corr, to_port: 0 },
            Edge { from_node: corr, from_port: 0, to_node: qual, to_port: 0 },
            Edge { from_node: corr, from_port: 0, to_node: cool, to_port: 0 },
            Edge { from_node: cool, from_port: 0, to_node: apply, to_port: 0 },
            Edge { from_node: qual, from_port: 0, to_node: apply, to_port: 1 },
            Edge { from_node: corr, from_port: 0, to_node: apply, to_port: 2 },
            Edge { from_node: apply, from_port: 0, to_node: out, to_port: 0 },
        ],
        out,
    )
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

/// A frame with hue across and brightness down, so the qualifier has
/// something to key and the clarity blur has detail to work on.
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
fn where_a_secondarys_frame_time_goes() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_target(false)
        .without_time()
        .try_init();

    let src = frame();
    let mut exec = GraphExecutor::new();
    for (name, g) in [
        ("primary only", primary_only()),
        ("two correctors", two_correctors()),
        ("primary + qualifier", primary_and_qualifier()),
        ("full secondary", full_secondary()),
    ] {
        // One run to warm caches and compile pipelines, then measure.
        let _ = exec.execute_linear(&src, &g, W, H, 0.0, 0, None, None);
        let mut best = f64::INFINITY;
        for _ in 0..3 {
            let t0 = std::time::Instant::now();
            let r = exec.execute_linear(&src, &g, W, H, 0.0, 0, None, None);
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            assert!(r.is_ok(), "{name} failed to execute");
            best = best.min(ms);
        }
        eprintln!("=== {name}: best of 3 = {best:.1} ms");
    }
}
