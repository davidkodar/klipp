//! Perf scenarios. Each one runs the grade executor (or other measured
//! unit) in a tight loop against synthetic frames and returns per-frame
//! wall-clock times.
//!
//! Only the grade-pipeline scenarios live here for now. Full-engine
//! scenarios (slider drag during real playback, watchdog, UI tick p99)
//! need a headless eframe + fixture video harness and are deferred until
//! after the eframe-wgpu port in phase 1 of the GPU-residency plan.

use std::collections::HashMap;
use std::time::Instant;

use klipp_nodes::{
    executor::GraphExecutor,
    graph::{Edge, Node, NodeGraph, NodeId, NodePos},
    kinds::NodeKind,
    params::{NodeKeyframeTracks, NodeParams},
};

use crate::{fixtures::gradient_frame_linear, stats::{summarise, ScenarioResult}};

/// Frame + warmup counts. Reduced from the 120 in the plan because the
/// current gpu.rs `apply_color_balance_gpu` (non-chain path) accumulates
/// descriptors in NVIDIA Vulkan's deferred-destruction queue and
/// triggers `device lost (Dropped)` after ~85 dispatches — documented
/// in the comment at `klipp-effects/src/gpu.rs:275`. Sizing to 60
/// keeps us inside the pre-drop window on the current codebase; plan B
/// phase 4 folds the non-chain dispatch into the chain infra which
/// already avoids the accumulation via `chain_bg_3binding`.
const FRAMES: usize = 60;
const WARMUP: usize = 10;

pub struct Scenario {
    pub name: &'static str,
    pub desc: &'static str,
    pub run: fn(bool) -> ScenarioResult,
}

pub const ALL: &[Scenario] = &[
    Scenario {
        name: "s01_colorbalance_1node_1080p",
        desc: "Single ColorBalance node at 1080p, 60 frames",
        run: s01_colorbalance_1node_1080p,
    },
    Scenario {
        name: "s02_grade_chain_5node_1080p",
        desc: "5-node grade chain at 1080p, 60 frames",
        run: s02_grade_chain_5node_1080p,
    },
    Scenario {
        name: "s03_grade_chain_10node_1080p",
        desc: "10-node grade chain at 1080p, 60 frames",
        run: s03_grade_chain_10node_1080p,
    },
];

// ─── Scenario implementations ───────────────────────────────────────

fn s01_colorbalance_1node_1080p(cpu_only: bool) -> ScenarioResult {
    let (w, h) = (1920u32, 1080u32);
    let graph = graph_colorbalance_only();
    run_grade_scenario("s01_colorbalance_1node_1080p", w, h, &graph, FRAMES, WARMUP, 4.0, cpu_only)
}

fn s02_grade_chain_5node_1080p(cpu_only: bool) -> ScenarioResult {
    let (w, h) = (1920u32, 1080u32);
    let graph = graph_chain_5node();
    run_grade_scenario("s02_grade_chain_5node_1080p", w, h, &graph, FRAMES, WARMUP, 8.0, cpu_only)
}

fn s03_grade_chain_10node_1080p(cpu_only: bool) -> ScenarioResult {
    let (w, h) = (1920u32, 1080u32);
    let graph = graph_chain_10node();
    run_grade_scenario("s03_grade_chain_10node_1080p", w, h, &graph, FRAMES, WARMUP, 15.0, cpu_only)
}

// ─── Shared runner ──────────────────────────────────────────────────

fn run_grade_scenario(
    name: &str,
    width: u32,
    height: u32,
    graph: &NodeGraph,
    frames: usize,
    warmup: usize,
    budget_p99_ms: f64,
    cpu_only: bool,
) -> ScenarioResult {
    let source = gradient_frame_linear(width, height);
    let mut exec = if cpu_only {
        GraphExecutor::new_cpu_only()
    } else {
        GraphExecutor::new()
    };
    let mut samples = Vec::with_capacity(frames);

    for i in 0..frames {
        let t = Instant::now();
        let result = exec
            .execute_linear(&source, graph, width, height, 0.0, i as u64, None, None)
            .expect("execute_linear failed");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        samples.push(ms);
        // Prevent DCE.
        std::hint::black_box(result.0.len());
    }

    summarise(name, width, height, &samples, warmup, budget_p99_ms)
}

// ─── Graph fixtures ─────────────────────────────────────────────────
//
// Kept as small helper builders so each scenario's graph is one
// function we can read and reason about. Params use non-default values
// so nothing bypasses.

fn mk_node(id: NodeId, kind: NodeKind, label: &str, params: NodeParams) -> Node {
    Node {
        id,
        kind,
        label: label.into(),
        enabled: true,
        params,
        keyframes: NodeKeyframeTracks::default(),
        output_link: None,
        input_links: Vec::new(),
    }
}

fn mk_edge(from: NodeId, to: NodeId) -> Edge {
    Edge { from_node: from, from_port: 0, to_node: to, to_port: 0 }
}

fn finalise(nodes: Vec<Node>, edges: Vec<Edge>, output_id: NodeId) -> NodeGraph {
    let mut positions = HashMap::new();
    for (i, n) in nodes.iter().enumerate() {
        positions.insert(n.id, NodePos { x: (i as f32) * 180.0, y: 0.0 });
    }
    NodeGraph {
        nodes,
        edges,
        output_node_id: output_id,
        node_positions: positions,
        working_space_override: None,
    }
}

fn graph_colorbalance_only() -> NodeGraph {
    let source = NodeId::new_v4();
    let colorbal = NodeId::new_v4();
    let output = NodeId::new_v4();

    let mut p = NodeParams::default();
    p.hue = Some(0.15); // non-zero → forces execution
    p.global_mix = Some(1.0);

    let nodes = vec![
        mk_node(source, NodeKind::Source, "Source", NodeParams::default()),
        mk_node(colorbal, NodeKind::ColorBalance, "ColorBalance", p),
        mk_node(output, NodeKind::Output, "Output", NodeParams::default()),
    ];
    let edges = vec![mk_edge(source, colorbal), mk_edge(colorbal, output)];
    finalise(nodes, edges, output)
}

fn graph_chain_5node() -> NodeGraph {
    let source = NodeId::new_v4();
    let exposure = NodeId::new_v4();
    let contrast = NodeId::new_v4();
    let saturation = NodeId::new_v4();
    let wheels = NodeId::new_v4();
    let colorbal = NodeId::new_v4();
    let output = NodeId::new_v4();

    let mut p_exp = NodeParams::default();
    p_exp.exposure = Some(0.3);

    let mut p_con = NodeParams::default();
    p_con.contrast = Some(0.15);
    p_con.clarity = Some(0.10);

    let mut p_sat = NodeParams::default();
    p_sat.saturation = Some(1.10);
    p_sat.vibrance = Some(0.10);

    let mut p_wheel = NodeParams::default();
    p_wheel.lift_master = Some(0.02);
    p_wheel.gamma_master = Some(-0.05);
    p_wheel.gain_master = Some(0.03);

    let mut p_cb = NodeParams::default();
    p_cb.temperature = Some(0.05);
    p_cb.tint = Some(-0.02);
    p_cb.hue = Some(0.08);
    p_cb.global_mix = Some(1.0);

    let nodes = vec![
        mk_node(source, NodeKind::Source, "Source", NodeParams::default()),
        mk_node(exposure, NodeKind::Exposure, "Exposure", p_exp),
        mk_node(contrast, NodeKind::Contrast, "Contrast", p_con),
        mk_node(saturation, NodeKind::Saturation, "Saturation", p_sat),
        mk_node(wheels, NodeKind::ColorWheels, "Wheels", p_wheel),
        mk_node(colorbal, NodeKind::ColorBalance, "ColorBalance", p_cb),
        mk_node(output, NodeKind::Output, "Output", NodeParams::default()),
    ];
    let edges = vec![
        mk_edge(source, exposure),
        mk_edge(exposure, contrast),
        mk_edge(contrast, saturation),
        mk_edge(saturation, wheels),
        mk_edge(wheels, colorbal),
        mk_edge(colorbal, output),
    ];
    finalise(nodes, edges, output)
}

fn graph_chain_10node() -> NodeGraph {
    // Extends the 5-node chain with 5 more GPU-capable filter nodes.
    let source = NodeId::new_v4();
    let exposure = NodeId::new_v4();
    let tone = NodeId::new_v4();
    let wheels = NodeId::new_v4();
    let wheel_sb = NodeId::new_v4();
    let contrast = NodeId::new_v4();
    let saturation = NodeId::new_v4();
    let curves = NodeId::new_v4();
    let huesat = NodeId::new_v4();
    let warper = NodeId::new_v4();
    let colorbal = NodeId::new_v4();
    let output = NodeId::new_v4();

    let mut p_exp = NodeParams::default();
    p_exp.exposure = Some(0.3);

    let mut p_tone = NodeParams::default();
    p_tone.highlights = Some(-0.2);
    p_tone.shadows = Some(0.2);
    p_tone.whites = Some(0.05);
    p_tone.blacks = Some(-0.05);

    let mut p_wheels = NodeParams::default();
    p_wheels.lift_master = Some(0.02);
    p_wheels.gamma_master = Some(-0.05);
    p_wheels.gain_master = Some(0.03);

    let mut p_wheel_sb = NodeParams::default();
    p_wheel_sb.lift_sat = Some(1.02);
    p_wheel_sb.gain_sat = Some(1.05);
    p_wheel_sb.gain_bri = Some(0.03);

    let mut p_con = NodeParams::default();
    p_con.contrast = Some(0.15);
    p_con.clarity = Some(0.10);

    let mut p_sat = NodeParams::default();
    p_sat.saturation = Some(1.10);
    p_sat.vibrance = Some(0.10);

    let p_curves = NodeParams::default(); // will be a bypass tunnel — still traversed
    let p_huesat = NodeParams::default();
    let p_warper = NodeParams::default();

    let mut p_cb = NodeParams::default();
    p_cb.temperature = Some(0.05);
    p_cb.tint = Some(-0.02);
    p_cb.hue = Some(0.08);
    p_cb.global_mix = Some(1.0);

    let nodes = vec![
        mk_node(source, NodeKind::Source, "Source", NodeParams::default()),
        mk_node(exposure, NodeKind::Exposure, "Exposure", p_exp),
        mk_node(tone, NodeKind::ToneControls, "Tone", p_tone),
        mk_node(wheels, NodeKind::ColorWheels, "Wheels", p_wheels),
        mk_node(wheel_sb, NodeKind::WheelSatBri, "WheelSB", p_wheel_sb),
        mk_node(contrast, NodeKind::Contrast, "Contrast", p_con),
        mk_node(saturation, NodeKind::Saturation, "Saturation", p_sat),
        mk_node(curves, NodeKind::RGBCurves, "Curves", p_curves),
        mk_node(huesat, NodeKind::HueSatCurves, "HueSat", p_huesat),
        mk_node(warper, NodeKind::ColorWarper, "Warper", p_warper),
        mk_node(colorbal, NodeKind::ColorBalance, "ColorBalance", p_cb),
        mk_node(output, NodeKind::Output, "Output", NodeParams::default()),
    ];
    let edges = vec![
        mk_edge(source, exposure),
        mk_edge(exposure, tone),
        mk_edge(tone, wheels),
        mk_edge(wheels, wheel_sb),
        mk_edge(wheel_sb, contrast),
        mk_edge(contrast, saturation),
        mk_edge(saturation, curves),
        mk_edge(curves, huesat),
        mk_edge(huesat, warper),
        mk_edge(warper, colorbal),
        mk_edge(colorbal, output),
    ];
    finalise(nodes, edges, output)
}
