//! A qualifier-driven secondary gives the same picture on the GPU as on
//! the CPU, and stays on the GPU while doing it.
//!
//! The executor recognises this graph shape and carries it end to end in
//! VRAM: the source's picture is kept in a side buffer, the Qualifier
//! writes its matte to `matte_buf`, and ApplyMatte blends the two
//! without the frame crossing the bus. That is invisible in the output —
//! a graph that round-trips every node produces the same pixels, only
//! far slower — so both halves are asserted here: the picture against
//! the CPU twins, and the residency against `last_run_chain_stats`.

use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId, NodePos};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 160;
const H: u32 = 120;

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

fn window() -> klipp_core::QualifierSettings {
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

/// The graph a secondary builds: a primary grade, a Qualifier keying it,
/// a second grade on the branch, and an ApplyMatte recombining them.
fn secondary_graph(mix: Option<f32>) -> NodeGraph {
    let (src, primary, qual, branch, apply, out) = (
        NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4(),
        NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4(),
    );
    let nodes = vec![
        node(src, NodeKind::Source, Default::default()),
        node(primary, NodeKind::Corrector, NodeParams {
            temperature: Some(-15.0),
            contrast: Some(0.03),
            ..Default::default()
        }),
        node(qual, NodeKind::Qualifier, NodeParams { qualifier: Some(window()), ..Default::default() }),
        node(branch, NodeKind::Corrector, NodeParams { temperature: Some(-45.0), ..Default::default() }),
        node(apply, NodeKind::ApplyMatte, NodeParams { mix, ..Default::default() }),
        node(out, NodeKind::Output, Default::default()),
    ];
    let edges = vec![
        Edge { from_node: src, from_port: 0, to_node: primary, to_port: 0 },
        Edge { from_node: primary, from_port: 0, to_node: qual, to_port: 0 },
        Edge { from_node: primary, from_port: 0, to_node: branch, to_port: 0 },
        Edge { from_node: branch, from_port: 0, to_node: apply, to_port: 0 },
        Edge { from_node: qual, from_port: 0, to_node: apply, to_port: 1 },
        Edge { from_node: primary, from_port: 0, to_node: apply, to_port: 2 },
        Edge { from_node: apply, from_port: 0, to_node: out, to_port: 0 },
    ];
    let node_positions = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id, NodePos { x: i as f32 * 200.0, y: 0.0 }))
        .collect();
    NodeGraph {
        output_node_id: out,
        nodes,
        edges,
        node_positions,
        working_space_override: None,
        frames: Vec::new(),
        sticky_notes: Vec::new(),
    }
}

/// Hue across, brightness down: the key selects a band of it, so the
/// frame holds selected, excluded and feathered pixels at once.
fn frame() -> Vec<f32> {
    let mut px = vec![0.0f32; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let t = x as f32 / W as f32;
            let v = 0.1 + 0.8 * (y as f32 / H as f32);
            px[i] = v * (0.55 + 0.45 * t);
            px[i + 1] = v * (0.45 + 0.25 * (1.0 - t));
            px[i + 2] = v * (0.35 + 0.5 * (1.0 - t));
            px[i + 3] = 1.0;
        }
    }
    px
}

fn run(graph: &NodeGraph, gpu: bool) -> (Vec<f32>, klipp_nodes::executor::ChainStats) {
    let mut exec = if gpu { GraphExecutor::new() } else { GraphExecutor::new_cpu_only() };
    let out = exec
        .execute_linear(&frame(), graph, W, H, 0.0, 0, None, None)
        .expect("graph runs")
        .0;
    (out, exec.last_run_chain_stats())
}

#[test]
fn a_secondary_matches_the_cpu_and_stays_on_the_gpu() {
    let graph = secondary_graph(Some(1.0));
    let (cpu, cpu_stats) = run(&graph, false);
    let (gpu, gpu_stats) = run(&graph, true);

    if gpu_stats.extends == 0 && gpu_stats.flushes == 0 {
        eprintln!("skipping: no usable GPU");
        return;
    }
    assert_eq!(cpu_stats.extends, 0, "the CPU executor cannot extend a chain");

    // Residency: the Qualifier, the branch grade and the ApplyMatte are
    // all chain dispatches, so only the Source runs per-node on the CPU
    // and the frame is read back once, at the end.
    assert!(
        gpu_stats.extends >= 3,
        "the secondary should ride the chain; stats {gpu_stats:?}",
    );
    assert_eq!(gpu_stats.flushes, 1, "one readback, at the end: {gpu_stats:?}");
    assert!(
        gpu_stats.cpu_nodes <= 1,
        "only the Source should touch the per-node CPU path: {gpu_stats:?}",
    );

    // And the picture. The CPU twins and the shaders are separate
    // implementations of the key and the blend, so this is the real
    // check that the fast shape did not change the result.
    assert_eq!(cpu.len(), gpu.len());
    let worst = cpu
        .chunks_exact(4)
        .zip(gpu.chunks_exact(4))
        .map(|(c, g)| (0..3).map(|i| (c[i] - g[i]).abs()).fold(0.0f32, f32::max))
        .fold(0.0f32, f32::max);
    let mean = cpu
        .chunks_exact(4)
        .zip(gpu.chunks_exact(4))
        .map(|(c, g)| (0..3).map(|i| (c[i] - g[i]).abs()).sum::<f32>() / 3.0)
        .sum::<f32>()
        / (W * H) as f32;
    eprintln!("secondary CPU vs GPU: worst {worst:e} mean {mean:e} linear");
    // A fifth of a code at 8 bits. The two paths compute the same
    // window and the same lerp; what differs is float order and the
    // qualifier's transcendentals.
    assert!(worst < 0.001, "CPU and GPU disagree by {worst} (mean {mean})");

    // The secondary must actually be doing something, or the comparison
    // above would pass on two identical no-ops.
    let (plain, _) = run(&secondary_graph(Some(0.0)), true);
    let changed = plain
        .chunks_exact(4)
        .zip(gpu.chunks_exact(4))
        .map(|(p, g)| (0..3).map(|i| (p[i] - g[i]).abs()).fold(0.0f32, f32::max))
        .fold(0.0f32, f32::max);
    assert!(changed > 0.01, "mix 1.0 and mix 0.0 should differ; got {changed}");
}

/// `mix` dials the whole secondary back without touching the key, and it
/// has to mean the same thing on both paths.
#[test]
fn the_secondarys_mix_agrees_on_both_paths() {
    for mix in [0.0f32, 0.35, 1.0] {
        let graph = secondary_graph(Some(mix));
        let (cpu, _) = run(&graph, false);
        let (gpu, stats) = run(&graph, true);
        if stats.extends == 0 && stats.flushes == 0 {
            eprintln!("skipping: no usable GPU");
            return;
        }
        let worst = cpu
            .chunks_exact(4)
            .zip(gpu.chunks_exact(4))
            .map(|(c, g)| (0..3).map(|i| (c[i] - g[i]).abs()).fold(0.0f32, f32::max))
            .fold(0.0f32, f32::max);
        eprintln!("mix {mix}: worst {worst:e}");
        assert!(worst < 0.001, "mix {mix} differs by {worst}");
    }
}
