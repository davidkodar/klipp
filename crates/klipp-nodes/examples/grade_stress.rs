//! Grade pipeline stress benchmark — runs the full default node graph
//! against synthetic frames at production resolution and reports per-
//! node ms / chain stats. Lets us iterate on executor perf without
//! firing up the app.
//!
//! Run:
//!   cargo run --release --example grade_stress -p klipp-nodes -- 1080p 200
//!
//! Args: [resolution] [iterations]
//!   resolution: 720p | 1080p | 4k     (default: 1080p)
//!   iterations: integer               (default: 100)
//!
//! The first 10 frames are treated as warm-up (pipeline compile +
//! GPU resource alloc dominate); steady-state is reported separately.

use std::time::Instant;

use klipp_nodes::executor::GraphExecutor;
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId, NodePos};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::{NodeKeyframeTracks, NodeParams};

fn main() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_target(false)
        .try_init();

    let args: Vec<String> = std::env::args().collect();
    let res_arg = args.get(1).cloned().unwrap_or_else(|| "1080p".into());
    let iters: usize = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);
    let (width, height) = match res_arg.as_str() {
        "720p" => (1280u32, 720u32),
        "1080p" => (1920u32, 1080u32),
        "4k" | "2160p" => (3840u32, 2160u32),
        other => {
            eprintln!("unknown resolution '{other}' — use 720p / 1080p / 4k");
            std::process::exit(2);
        }
    };

    println!("=== grade_stress ===");
    println!("resolution: {width}x{height}");
    println!("iterations: {iters} (10 warm-up + {} steady)", iters.saturating_sub(10));

    // Build a realistic graded node graph — every supported chain node
    // active with non-default params so nothing bypasses.
    let graph = realistic_grade_graph();
    println!("nodes: {}", graph.nodes.len());

    // Synthetic source: a deterministic gradient so caches that key on
    // content (LUTs, etc.) behave the same each frame.
    let source = make_gradient_frame(width, height);

    let mut exec = GraphExecutor::new();
    let gpu_present = std::env::var("RUST_LOG").is_ok();
    let _ = gpu_present;

    let mut times_ms: Vec<f64> = Vec::with_capacity(iters);
    for i in 0..iters {
        let t = Instant::now();
        let result = exec
            .execute_linear(&source, &graph, width, height, 0.0, i as u64, None, None)
            .expect("execute_linear failed");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        times_ms.push(ms);
        // Touch result so it's not optimised away.
        std::hint::black_box(result.0.len());
    }

    let warmup = times_ms.iter().take(10).copied().collect::<Vec<_>>();
    let steady = times_ms.iter().skip(10).copied().collect::<Vec<_>>();

    fn stats(label: &str, v: &[f64]) {
        if v.is_empty() {
            println!("{label}: (empty)");
            return;
        }
        let min = v.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let avg = v.iter().sum::<f64>() / v.len() as f64;
        let mut sorted = v.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p50 = sorted[sorted.len() / 2];
        let p95 = sorted[(sorted.len() * 95) / 100];
        let fps = if avg > 0.0 { 1000.0 / avg } else { 0.0 };
        println!(
            "{label:>10}: n={:3}  min={:5.1}  p50={:5.1}  avg={:5.1}  p95={:5.1}  max={:5.1}  ({:.1} fps)",
            v.len(), min, p50, avg, p95, max, fps,
        );
    }

    println!();
    stats("warmup", &warmup);
    stats("steady", &steady);
    println!();
    println!("Per-node breakdown:  RUST_LOG=klipp_nodes=debug cargo run ... ");
    println!("(debug-level PERF graph_exec lines show per-frame node ms.)");
}

fn realistic_grade_graph() -> NodeGraph {
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

    fn n(id: NodeId, kind: NodeKind, label: &str, params: NodeParams) -> Node {
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
    fn e(from: NodeId, to: NodeId) -> Edge {
        Edge { from_node: from, from_port: 0, to_node: to, to_port: 0 }
    }

    let mut p_exposure = NodeParams::default();
    p_exposure.exposure = Some(0.3);

    let mut p_tone = NodeParams::default();
    p_tone.highlights = Some(-0.2);
    p_tone.shadows = Some(0.2);
    p_tone.whites = Some(0.05);
    p_tone.blacks = Some(-0.05);

    let mut p_wheels = NodeParams::default();
    p_wheels.lift_master = Some(0.02);
    p_wheels.gamma_master = Some(-0.05);
    p_wheels.gain_master = Some(0.03);
    p_wheels.lift_x = Some(0.01);
    p_wheels.gamma_y = Some(-0.01);

    let mut p_wheel_sb = NodeParams::default();
    p_wheel_sb.lift_sat = Some(1.02);
    p_wheel_sb.gain_sat = Some(1.05);
    p_wheel_sb.gain_bri = Some(0.03);

    let mut p_contrast = NodeParams::default();
    p_contrast.contrast = Some(0.15);
    p_contrast.clarity = Some(0.10);

    let mut p_saturation = NodeParams::default();
    p_saturation.saturation = Some(1.10);
    p_saturation.vibrance = Some(0.10);

    let p_curves = NodeParams::default(); // Identity → bypass tunnel
    let p_huesat = NodeParams::default(); // Identity → bypass tunnel
    let p_warper = NodeParams::default(); // Identity → bypass tunnel

    let mut p_colorbal = NodeParams::default();
    p_colorbal.temperature = Some(0.05);
    p_colorbal.tint = Some(-0.02);
    p_colorbal.global_mix = Some(1.0);

    let nodes = vec![
        n(source, NodeKind::Source, "Source", NodeParams::default()),
        n(exposure, NodeKind::Exposure, "Exposure", p_exposure),
        n(tone, NodeKind::ToneControls, "Tone", p_tone),
        n(wheels, NodeKind::ColorWheels, "Wheels", p_wheels),
        n(wheel_sb, NodeKind::WheelSatBri, "WheelSB", p_wheel_sb),
        n(contrast, NodeKind::Contrast, "Contrast", p_contrast),
        n(saturation, NodeKind::Saturation, "Saturation", p_saturation),
        n(curves, NodeKind::RGBCurves, "Curves", p_curves),
        n(huesat, NodeKind::HueSatCurves, "HueSat", p_huesat),
        n(warper, NodeKind::ColorWarper, "Warper", p_warper),
        n(colorbal, NodeKind::ColorBalance, "ColorBalance", p_colorbal),
        n(output, NodeKind::Output, "Output", NodeParams::default()),
    ];

    let edges = vec![
        e(source, exposure),
        e(exposure, tone),
        e(tone, wheels),
        e(wheels, wheel_sb),
        e(wheel_sb, contrast),
        e(contrast, saturation),
        e(saturation, curves),
        e(curves, huesat),
        e(huesat, warper),
        e(warper, colorbal),
        e(colorbal, output),
    ];

    let mut node_positions = std::collections::HashMap::new();
    for (i, node) in nodes.iter().enumerate() {
        node_positions.insert(node.id, NodePos { x: i as f32 * 200.0, y: 0.0 });
    }

    NodeGraph { nodes, edges, output_node_id: output, node_positions, working_space_override: None }
}

fn make_gradient_frame(w: u32, h: u32) -> Vec<f32> {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let r = (x as f32 / w as f32).clamp(0.0, 1.0);
            let g = (y as f32 / h as f32).clamp(0.0, 1.0);
            let b = (((x + y) as f32 / (w + h) as f32) * 0.7).clamp(0.0, 1.0);
            // Source frame is linear-light per execute_linear's contract.
            out.push(r);
            out.push(g);
            out.push(b);
            out.push(1.0);
        }
    }
    out
}
