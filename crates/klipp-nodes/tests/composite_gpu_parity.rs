//! Blend and Layer: the shader has to agree with the CPU arm.
//!
//! Both nodes now have two implementations — the per-node CPU path in
//! `executor.rs` and a chain dispatch in `klipp-effects` — and they are
//! separate code, not one calling the other. These tests run the same
//! graph through both executors and compare the picture, the alpha
//! included, because Layer's whole point is that the matte survives.
//!
//! They also assert residency: a composite that quietly fell back to
//! the CPU would still match, and the match would mean nothing.

use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId, NodePos};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 320;
const H: u32 = 180;

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

fn grade() -> NodeParams {
    NodeParams {
        temperature: Some(-18.0),
        contrast: Some(0.08),
        shadows: Some(0.01),
        ..Default::default()
    }
}

/// `Source ─┬─► Corrector ─► [composite].0`
///         `└──────────────► [composite].1`
///
/// Port 1 is the bare source, so the picture has to be kept at chain
/// start — before the Corrector's first dispatch overwrites the slot.
fn composite_over_source(kind: NodeKind, params: NodeParams) -> NodeGraph {
    let (src, corr, mix, out) =
        (NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4());
    graph(
        vec![
            node(src, NodeKind::Source, Default::default()),
            node(corr, NodeKind::Corrector, grade()),
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

/// Two graded branches recombined — port 1 is a chain tail in its own
/// right, so it is kept on the extend path instead.
fn blend_two_branches(mix: f32) -> NodeGraph {
    let (src, warm, cool, blend, out) = (
        NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4(),
        NodeId::new_v4(), NodeId::new_v4(),
    );
    graph(
        vec![
            node(src, NodeKind::Source, Default::default()),
            node(warm, NodeKind::Corrector, grade()),
            node(cool, NodeKind::Corrector, NodeParams { temperature: Some(-60.0), ..Default::default() }),
            node(blend, NodeKind::Blend, NodeParams { mix: Some(mix), ..Default::default() }),
            node(out, NodeKind::Output, Default::default()),
        ],
        vec![
            Edge { from_node: src, from_port: 0, to_node: warm, to_port: 0 },
            Edge { from_node: src, from_port: 0, to_node: cool, to_port: 0 },
            Edge { from_node: warm, from_port: 0, to_node: blend, to_port: 0 },
            Edge { from_node: cool, from_port: 0, to_node: blend, to_port: 1 },
            Edge { from_node: blend, from_port: 0, to_node: out, to_port: 0 },
        ],
        out,
    )
}

/// Hue across, brightness down, and alpha ramping left to right so the
/// over operator has a real range to composite rather than a flat 1.
fn frame() -> Vec<f32> {
    let mut px = vec![0.0f32; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let t = x as f32 / (W - 1) as f32;
            let v = 0.12 + 0.8 * (y as f32 / H as f32);
            px[i] = v * (0.6 + 0.4 * t);
            px[i + 1] = v * (0.45 + 0.25 * (1.0 - t));
            px[i + 2] = v * (0.35 + 0.5 * (1.0 - t));
            px[i + 3] = t;
        }
    }
    px
}

fn run(g: &NodeGraph, gpu: bool) -> (Vec<f32>, klipp_nodes::executor::ChainStats) {
    let mut exec = if gpu { GraphExecutor::new() } else { GraphExecutor::new_cpu_only() };
    let out = exec
        .execute_linear(&frame(), g, W, H, 0.0, 0, None, None)
        .expect("graph runs")
        .0;
    (out, exec.last_run_chain_stats())
}

/// Worst absolute disagreement over all four channels.
fn worst_rgba(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .map(|(x, y)| (0..4).map(|i| (x[i] - y[i]).abs()).fold(0.0f32, f32::max))
        .fold(0.0f32, f32::max)
}

fn check(name: &str, g: &NodeGraph) {
    let (cpu, cpu_stats) = run(g, false);
    let (gpu, gpu_stats) = run(g, true);

    if gpu_stats.extends == 0 && gpu_stats.flushes == 0 {
        eprintln!("skipping {name}: no usable GPU");
        return;
    }
    assert_eq!(cpu_stats.extends, 0, "the CPU executor cannot extend a chain");

    // The composite itself must be a chain dispatch, not a CPU node
    // that happens to agree. Source is the one allowed CPU node.
    assert!(
        gpu_stats.cpu_nodes <= 1,
        "{name}: the composite should ride the chain, not fall back: {gpu_stats:?}",
    );

    let worst = worst_rgba(&cpu, &gpu);
    eprintln!("{name}: worst CPU vs GPU = {worst:e} linear ({gpu_stats:?})");
    assert!(worst < 0.001, "{name}: CPU and GPU disagree by {worst}");
}

#[test]
fn a_blend_over_the_source_matches_the_cpu() {
    check(
        "blend over source",
        &composite_over_source(NodeKind::Blend, NodeParams { mix: Some(0.35), ..Default::default() }),
    );
}

#[test]
fn a_layer_over_the_source_matches_the_cpu() {
    check(
        "layer over source",
        &composite_over_source(NodeKind::Layer, Default::default()),
    );
}

#[test]
fn a_blend_of_two_graded_branches_matches_the_cpu() {
    check("blend two branches", &blend_two_branches(0.5));
}

/// The comparisons above would pass on two identical no-ops, so this
/// pins that the mix parameter actually moves the picture on the GPU
/// path and that the endpoints are the two inputs.
#[test]
fn the_mix_parameter_reaches_the_shader() {
    let at = |m: f32| {
        run(&composite_over_source(NodeKind::Blend, NodeParams { mix: Some(m), ..Default::default() }), true).0
    };
    let (zero, half, one) = (at(0.0), at(0.5), at(1.0));
    if zero.is_empty() {
        return;
    }
    let spread = worst_rgba(&zero, &one);
    assert!(spread > 0.01, "mix 0 and mix 1 should differ; got {spread}");

    // mix 0 is port 0 (the graded picture), mix 1 is port 1 (the
    // source), and half is the midpoint of the two.
    let midpoint: Vec<f32> = zero.iter().zip(one.iter()).map(|(a, b)| (a + b) * 0.5).collect();
    let off = worst_rgba(&half, &midpoint);
    assert!(off < 0.001, "half mix should be the midpoint of the ends; off by {off}");
}

/// Layer's alpha is the over operator's, not a saturated 1. With the
/// source alpha ramping 0 → 1 the composite must ramp with it.
#[test]
fn layer_composites_the_alpha_rather_than_saturating_it() {
    let (out, stats) = run(&composite_over_source(NodeKind::Layer, Default::default()), true);
    if stats.extends == 0 && stats.flushes == 0 {
        return;
    }
    let src = frame();
    for px in (0..(W * H) as usize).step_by(37) {
        let a = src[px * 4 + 3];
        // Foreground and background are the same picture here, so both
        // alphas are `a` and the over operator gives a + a(1 - a).
        let want = a + a * (1.0 - a);
        let got = out[px * 4 + 3];
        assert!(
            (got - want).abs() < 1e-3,
            "pixel {px}: over alpha should be {want}, got {got}",
        );
    }
    let last = out[((W * H - 1) * 4 + 3) as usize];
    assert!(last <= 1.0 + 1e-6, "alpha must not run past 1: {last}");
}

/// Several composites in one graph, sharing it with the two other
/// features that keep a picture on the GPU: the Corrector's own
/// Keep/Mix pair (`global_mix`) and a qualifier secondary.
///
/// This is the ordering test. A composite's kept picture is written by
/// a queued upload in some routes and by an in-encoder copy in others
/// (pool slots, M3 step 17b), and the
/// two land at different points relative to the dispatches that read
/// them. The executor's rule — one queued upload per chain segment,
/// in-encoder keeps unrestricted — is what keeps that straight, and
/// the only honest check is whether the picture still matches the CPU.
///
/// No residency assertion: whatever the scheduler decides here, a
/// composite that falls back to the CPU is slower and still correct.
/// What must not happen is a composite reading someone else's picture.
///
/// It is also the regression test for the keyer gate. This graph used
/// to panic in the secondary's keyer with "keep_chain_input first": a
/// secondary whose source had run on the CPU still let its
/// keyer dispatch, because the gate asked only whether *a* chain was
/// live, not whether the kept buffer held this secondary's source.
/// First frame it panicked; later frames it would have keyed the
/// previous frame's picture instead, which is worse.
#[test]
fn composites_alongside_the_other_kept_pictures_match_the_cpu() {
    let (src, a, b, c) =
        (NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4());
    let (blend1, blend2, layer) = (NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4());
    let (qual, sec, apply, out) =
        (NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4());

    let window = klipp_core::QualifierSettings {
        enabled: true,
        hue_center: 27.0,
        hue_width: 22.0,
        hue_soft: 10.0,
        sat_low: 0.15,
        sat_high: 0.6,
        sat_soft: 0.05,
        lum_low: 0.1,
        lum_high: 0.9,
        lum_soft: 0.04,
        ..Default::default()
    };

    let g = graph(
        vec![
            node(src, NodeKind::Source, Default::default()),
            // A Corrector that uses its own Keep/Mix pair on grade_mix_src.
            node(a, NodeKind::Corrector, NodeParams { global_mix: Some(0.7), ..grade() }),
            node(b, NodeKind::Corrector, NodeParams { temperature: Some(-55.0), ..Default::default() }),
            node(c, NodeKind::Corrector, NodeParams { saturation: Some(1.4), ..Default::default() }),
            // First composite: port 1 is the bare source.
            node(blend1, NodeKind::Blend, NodeParams { mix: Some(0.4), ..Default::default() }),
            // Second composite: port 1 is a different graded branch.
            node(blend2, NodeKind::Blend, NodeParams { mix: Some(0.6), ..Default::default() }),
            // A Layer too, over a third branch.
            node(layer, NodeKind::Layer, Default::default()),
            // And a qualifier secondary, which keeps its own picture.
            node(qual, NodeKind::Qualifier, NodeParams { qualifier: Some(window), ..Default::default() }),
            node(sec, NodeKind::Corrector, NodeParams { temperature: Some(30.0), ..Default::default() }),
            node(apply, NodeKind::ApplyMatte, Default::default()),
            node(out, NodeKind::Output, Default::default()),
        ],
        vec![
            Edge { from_node: src, from_port: 0, to_node: a, to_port: 0 },
            Edge { from_node: a, from_port: 0, to_node: blend1, to_port: 0 },
            Edge { from_node: src, from_port: 0, to_node: blend1, to_port: 1 },
            Edge { from_node: src, from_port: 0, to_node: b, to_port: 0 },
            Edge { from_node: blend1, from_port: 0, to_node: blend2, to_port: 0 },
            Edge { from_node: b, from_port: 0, to_node: blend2, to_port: 1 },
            Edge { from_node: src, from_port: 0, to_node: c, to_port: 0 },
            Edge { from_node: blend2, from_port: 0, to_node: layer, to_port: 0 },
            Edge { from_node: c, from_port: 0, to_node: layer, to_port: 1 },
            // Secondary keyed off the composite result.
            Edge { from_node: layer, from_port: 0, to_node: qual, to_port: 0 },
            Edge { from_node: layer, from_port: 0, to_node: sec, to_port: 0 },
            Edge { from_node: sec, from_port: 0, to_node: apply, to_port: 0 },
            Edge { from_node: qual, from_port: 0, to_node: apply, to_port: 1 },
            Edge { from_node: layer, from_port: 0, to_node: apply, to_port: 2 },
            Edge { from_node: apply, from_port: 0, to_node: out, to_port: 0 },
        ],
        out,
    );

    let (cpu, _) = run(&g, false);
    let (gpu, stats) = run(&g, true);
    if stats.extends == 0 && stats.flushes == 0 {
        eprintln!("skipping: no usable GPU");
        return;
    }
    let worst = worst_rgba(&cpu, &gpu);
    eprintln!("mixed graph: worst CPU vs GPU = {worst:e} linear ({stats:?})");
    assert!(worst < 0.001, "CPU and GPU disagree by {worst} ({stats:?})");
}
