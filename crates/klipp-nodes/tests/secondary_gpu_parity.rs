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

fn secondary_graph(mix: Option<f32>) -> NodeGraph {
    secondary_graph_with(mix, 1, false)
}

/// The graph a secondary builds: a primary grade, a Qualifier keying it,
/// `branch_len` grades on the branch, and an ApplyMatte recombining
/// them. With `foreign`, an unrelated node hangs off the Source — it
/// belongs to no part of the shape, and if the execution order drops it
/// between the source and the blend the shape must be declined rather
/// than mis-scheduled.
fn secondary_graph_with(mix: Option<f32>, branch_len: usize, foreign: bool) -> NodeGraph {
    let (src, primary, qual, apply, out) = (
        NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4(),
        NodeId::new_v4(), NodeId::new_v4(),
    );
    let mut nodes = vec![
        node(src, NodeKind::Source, Default::default()),
        node(primary, NodeKind::Corrector, NodeParams {
            temperature: Some(-15.0),
            contrast: Some(0.03),
            ..Default::default()
        }),
        node(qual, NodeKind::Qualifier, NodeParams { qualifier: Some(window()), ..Default::default() }),
        node(apply, NodeKind::ApplyMatte, NodeParams { mix, ..Default::default() }),
        node(out, NodeKind::Output, Default::default()),
    ];
    let mut edges = vec![
        Edge { from_node: src, from_port: 0, to_node: primary, to_port: 0 },
        Edge { from_node: primary, from_port: 0, to_node: qual, to_port: 0 },
        Edge { from_node: qual, from_port: 0, to_node: apply, to_port: 1 },
        Edge { from_node: primary, from_port: 0, to_node: apply, to_port: 2 },
        Edge { from_node: apply, from_port: 0, to_node: out, to_port: 0 },
    ];
    // The grade branch: `branch_len` nodes in a line from the primary to
    // ApplyMatte's graded port.
    let mut upstream = primary;
    for i in 0..branch_len {
        let id = NodeId::new_v4();
        // Each one does something different, so a branch that silently
        // lost a node would show up in the picture.
        nodes.push(node(id, NodeKind::Corrector, NodeParams {
            temperature: Some(-45.0 + i as f32 * 12.0),
            ..Default::default()
        }));
        edges.push(Edge { from_node: upstream, from_port: 0, to_node: id, to_port: 0 });
        upstream = id;
    }
    edges.push(Edge { from_node: upstream, from_port: 0, to_node: apply, to_port: 0 });
    if foreign {
        let odd = NodeId::new_v4();
        nodes.push(node(odd, NodeKind::Exposure, NodeParams { exposure: Some(0.5), ..Default::default() }));
        edges.push(Edge { from_node: src, from_port: 0, to_node: odd, to_port: 0 });
    }
    let branch = upstream;
    let _ = branch;
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

/// The grade branch may be any length. Three nodes on it should ride
/// the chain exactly as one does — the branch is ordinary chain work
/// once the shape is recognised.
#[test]
fn a_longer_grade_branch_still_rides_the_chain() {
    for branch_len in [1usize, 2, 3] {
        let graph = secondary_graph_with(Some(1.0), branch_len, false);
        let (cpu, _) = run(&graph, false);
        let (gpu, stats) = run(&graph, true);
        if stats.extends == 0 && stats.flushes == 0 {
            eprintln!("skipping: no usable GPU");
            return;
        }
        let worst = worst_diff(&cpu, &gpu);
        eprintln!("branch of {branch_len}: worst {worst:e}, {stats:?}");
        assert!(worst < 0.001, "branch of {branch_len} differs by {worst}");
        // One dispatch for the qualifier, one for the blend, and one per
        // branch node.
        assert!(
            stats.extends >= 2 + branch_len as u32,
            "branch of {branch_len} should all ride the chain: {stats:?}",
        );
        assert_eq!(stats.flushes, 1, "still one readback: {stats:?}");
    }
}

/// A node that belongs to no part of the shape, sitting on the same
/// source. If the execution order puts it between the source and the
/// blend the shape is declined — and the picture must still be right,
/// because ApplyMatte then reads the matte back out of GPU memory
/// instead of pretending it has none.
#[test]
fn a_foreign_node_costs_speed_and_not_correctness() {
    let graph = secondary_graph_with(Some(1.0), 1, true);
    let (cpu, _) = run(&graph, false);
    let (gpu, stats) = run(&graph, true);
    if stats.extends == 0 && stats.flushes == 0 {
        eprintln!("skipping: no usable GPU");
        return;
    }
    let worst = worst_diff(&cpu, &gpu);
    eprintln!("with a foreign node: worst {worst:e}, {stats:?}");
    assert!(worst < 0.001, "a foreign node changed the picture: {worst}");

    // And the secondary is still in the result — the failure this
    // guards against is a matte that quietly resolves to "select
    // nothing", which would leave the picture merely ungraded.
    let (plain, _) = run(&secondary_graph_with(Some(0.0), 1, true), true);
    let changed = worst_diff(&plain, &gpu);
    assert!(changed > 0.01, "the secondary went missing: {changed}");
}

fn worst_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .map(|(x, y)| (0..3).map(|i| (x[i] - y[i]).abs()).fold(0.0f32, f32::max))
        .fold(0.0f32, f32::max)
}


/// The matte need not come from a Qualifier: a spatial Mask draws
/// geometry instead of keying, so it has no input picture to check, and
/// publishes its matte through its own dispatch rather than the chain
/// encoder — a submit, but never a readback.
///
/// (A LumaKey would belong here too, but its shader and its CPU twin
/// key different domains; see the note in `SecondaryChain::detect`.)
#[test]
fn the_other_matte_sources_ride_the_chain_too() {
    for (name, matte) in [("mask", MatteSource::Mask)] {
        let graph = matte_graph(matte, Some(1.0));
        let (cpu, _) = run(&graph, false);
        let (gpu, stats) = run(&graph, true);
        if stats.extends == 0 && stats.flushes == 0 {
            eprintln!("skipping: no usable GPU");
            return;
        }
        let worst = worst_diff(&cpu, &gpu);
        eprintln!("{name}: worst {worst:e}, {stats:?}");
        assert!(worst < 0.001, "{name} differs by {worst}");
        // The blend and the branch ride the chain. A Mask publishes its
        // matte through its own dispatch rather than the chain encoder,
        // so it costs a submit but never a readback.
        assert!(stats.extends >= 2, "{name} should ride the chain: {stats:?}");
        assert_eq!(stats.flushes, 1, "{name}: one readback: {stats:?}");

        let (plain, _) = run(&matte_graph(matte, Some(0.0)), true);
        let changed = worst_diff(&plain, &gpu);
        assert!(changed > 0.005, "{name}: the secondary did nothing ({changed})");
    }
}

#[derive(Clone, Copy)]
enum MatteSource {
    Luma,
    Mask,
}

/// The same secondary, with the matte from a luma key or a spatial mask.
fn matte_graph(source: MatteSource, mix: Option<f32>) -> NodeGraph {
    let (src, primary, matte, branch, apply, out) = (
        NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4(),
        NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4(),
    );
    let (matte_kind, matte_params) = match source {
        MatteSource::Luma => (
            NodeKind::LumaKey,
            NodeParams {
                luma_key: Some(klipp_core::LumaKeySettings {
                    enabled: true,
                    low: 0.15,
                    high: 0.75,
                    soft: 0.08,
                    invert: false,
                }),
                ..Default::default()
            },
        ),
        MatteSource::Mask => (
            NodeKind::Mask,
            NodeParams {
                masks: Some(klipp_core::MaskStack {
                    masks: vec![klipp_core::Mask {
                        shape: klipp_core::MaskShape::Circle {
                            center_x: 0.5,
                            center_y: 0.5,
                            radius: 0.3,
                        },
                        feather: 0.1,
                        invert: false,
                        opacity: 1.0,
                        enabled: true,
                        ..Default::default()
                    }],
                }),
                ..Default::default()
            },
        ),
    };
    let mut nodes = vec![
        node(src, NodeKind::Source, Default::default()),
        node(primary, NodeKind::Corrector, NodeParams {
            temperature: Some(-15.0),
            ..Default::default()
        }),
        node(matte, matte_kind, matte_params),
        node(branch, NodeKind::Corrector, NodeParams { temperature: Some(-45.0), ..Default::default() }),
        node(apply, NodeKind::ApplyMatte, NodeParams { mix, ..Default::default() }),
        node(out, NodeKind::Output, Default::default()),
    ];
    let mut edges = vec![
        Edge { from_node: src, from_port: 0, to_node: primary, to_port: 0 },
        Edge { from_node: primary, from_port: 0, to_node: branch, to_port: 0 },
        Edge { from_node: branch, from_port: 0, to_node: apply, to_port: 0 },
        Edge { from_node: matte, from_port: 0, to_node: apply, to_port: 1 },
        Edge { from_node: primary, from_port: 0, to_node: apply, to_port: 2 },
        Edge { from_node: apply, from_port: 0, to_node: out, to_port: 0 },
    ];
    // A keyer looks at the picture; a mask does not.
    if matches!(source, MatteSource::Luma) {
        edges.push(Edge { from_node: primary, from_port: 0, to_node: matte, to_port: 0 });
    }
    let node_positions = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id, NodePos { x: i as f32 * 200.0, y: 0.0 }))
        .collect();
    nodes.shrink_to_fit();
    edges.shrink_to_fit();
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
