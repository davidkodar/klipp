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


/// The matte need not come from a Qualifier. A luminance key is the
/// simpler keyer and rides the chain the same way; a spatial Mask draws
/// geometry instead of keying, so it has no input picture to check, and
/// publishes its matte through its own dispatch rather than the chain
/// encoder — a submit, but never a readback.
#[test]
fn the_other_matte_sources_ride_the_chain_too() {
    // The luma key's tolerance is looser for one reason:
    // `generate_luma_key_matte`, its CPU twin, takes u8 and so keys a
    // quantised picture, while the shader keys the full-precision one.
    // A 1/255 step shifts where the soft falloff lands, which is about
    // a code and a half at 8 bits. The Qualifier's twin works in float
    // and needs no such allowance.
    for (name, matte, tolerance) in [
        ("luma key", MatteSource::Luma, 0.008f32),
        ("mask", MatteSource::Mask, 0.001),
        // The difference key's twins both work in linear float on the
        // same plate, so the only slack needed is the float order of the
        // picture that reaches them.
        ("difference key", MatteSource::Difference, 0.002),
    ] {
        let graph = matte_graph(matte, Some(1.0));
        let (cpu, _) = run(&graph, false);
        let (gpu, stats) = run(&graph, true);
        if stats.extends == 0 && stats.flushes == 0 {
            eprintln!("skipping: no usable GPU");
            return;
        }
        let worst = worst_diff(&cpu, &gpu);
        eprintln!("{name}: worst {worst:e}, {stats:?}");
        assert!(worst < tolerance, "{name} differs by {worst}");
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
    Difference,
}

/// A clean plate on disk, written once. The difference key needs a real
/// file because the node's plate pipeline is the thing under test as
/// much as the shader is: decode, resize to the frame, then key.
///
/// Deliberately not the frame's size, so the resize runs.
fn plate_path() -> &'static std::path::Path {
    static PLATE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    PLATE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("klipp-dk-parity-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("plate.png");
        // A horizontal ramp: it matches the frame nowhere exactly, so
        // the matte has real structure and a phase or addressing error
        // in the resize shows up as a shifted matte.
        let mut img = image::RgbaImage::new(W / 2, H / 2);
        for (x, _y, px) in img.enumerate_pixels_mut() {
            let v = (x * 255 / (W / 2 - 1)) as u8;
            *px = image::Rgba([v, 128, 255 - v, 255]);
        }
        img.save(&path).expect("write plate");
        path
    })
    .as_path()
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
        MatteSource::Difference => (
            NodeKind::DifferenceKey,
            NodeParams {
                difference_key: Some(klipp_core::DifferenceKeySettings {
                    enabled: true,
                    plate_path: Some(plate_path().to_path_buf()),
                    plate_frame: None,
                    tolerance: 0.02,
                    soft: 0.12,
                    invert: false,
                    match_space: klipp_core::DifferenceKeyMatchSpace::Rgb,
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
    if matches!(source, MatteSource::Luma | MatteSource::Difference) {
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

/// A second qualifier-driven window on the same clip — the shape a
/// colourist builds without thinking about it — matches the CPU and
/// rides the same chain as the first.
///
/// The detector used to stop at the first shape it found, so the second
/// keyer and blend fell back to the per-node CPU path and cost a flush
/// and an upload: at 1214x2160 two secondaries ran 41.3 ms against
/// 19.5 ms for one. Recognising both brought that to 21.8 ms.
///
/// No extra GPU buffers were needed, and the reason is worth stating
/// because it is what makes the change safe: the shapes are sequential,
/// not concurrent. The second one's ungraded port is the first one's
/// ApplyMatte, so by the time it wants the kept picture and `matte_buf`,
/// the first shape has finished with both. If they ever overlapped, one
/// kept buffer and one matte buffer would not be enough and this test
/// would show it as a wrong picture rather than as a slow one.
fn two_secondary_graph(second_mix: Option<f32>) -> NodeGraph {
    let (src, corr) = (NodeId::new_v4(), NodeId::new_v4());
    let (q1, c1, a1) = (NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4());
    let (q2, c2, a2) = (NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4());
    let out = NodeId::new_v4();
    // A second window somewhere else in hue, so the two mattes select
    // different pixels and a swapped or stale matte shows up.
    let other = klipp_core::QualifierSettings {
        enabled: true,
        hue_center: 200.0,
        hue_width: 60.0,
        hue_soft: 15.0,
        sat_low: 0.05,
        sat_high: 1.0,
        sat_soft: 0.05,
        lum_low: 0.05,
        lum_high: 1.0,
        lum_soft: 0.05,
        ..Default::default()
    };
    let nodes = vec![
        node(src, NodeKind::Source, Default::default()),
        node(corr, NodeKind::Corrector, NodeParams { contrast: Some(0.04), ..Default::default() }),
        node(q1, NodeKind::Qualifier, NodeParams { qualifier: Some(window()), ..Default::default() }),
        node(c1, NodeKind::Corrector, NodeParams { temperature: Some(-40.0), ..Default::default() }),
        node(a1, NodeKind::ApplyMatte, NodeParams { mix: Some(1.0), ..Default::default() }),
        node(q2, NodeKind::Qualifier, NodeParams { qualifier: Some(other), ..Default::default() }),
        node(c2, NodeKind::Corrector, NodeParams { saturation: Some(0.4), ..Default::default() }),
        node(a2, NodeKind::ApplyMatte, NodeParams { mix: second_mix, ..Default::default() }),
        node(out, NodeKind::Output, Default::default()),
    ];
    let edges = vec![
        Edge { from_node: src, from_port: 0, to_node: corr, to_port: 0 },
        Edge { from_node: corr, from_port: 0, to_node: q1, to_port: 0 },
        Edge { from_node: corr, from_port: 0, to_node: c1, to_port: 0 },
        Edge { from_node: c1, from_port: 0, to_node: a1, to_port: 0 },
        Edge { from_node: q1, from_port: 0, to_node: a1, to_port: 1 },
        Edge { from_node: corr, from_port: 0, to_node: a1, to_port: 2 },
        Edge { from_node: a1, from_port: 0, to_node: q2, to_port: 0 },
        Edge { from_node: a1, from_port: 0, to_node: c2, to_port: 0 },
        Edge { from_node: c2, from_port: 0, to_node: a2, to_port: 0 },
        Edge { from_node: q2, from_port: 0, to_node: a2, to_port: 1 },
        Edge { from_node: a1, from_port: 0, to_node: a2, to_port: 2 },
        Edge { from_node: a2, from_port: 0, to_node: out, to_port: 0 },
    ];
    let node_positions = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id, NodePos { x: i as f32 * 160.0, y: 0.0 }))
        .collect();
    NodeGraph {
        nodes,
        edges,
        output_node_id: out,
        node_positions,
        working_space_override: None,
        frames: Vec::new(),
        sticky_notes: Vec::new(),
    }
}

#[test]
fn two_stacked_secondaries_match_the_cpu_and_both_ride_the_chain() {
    let graph = two_secondary_graph(Some(1.0));
    let (cpu, cpu_stats) = run(&graph, false);
    let (gpu, gpu_stats) = run(&graph, true);

    if gpu_stats.extends == 0 && gpu_stats.flushes == 0 {
        eprintln!("skipping: no usable GPU");
        return;
    }
    assert_eq!(cpu_stats.extends, 0, "the CPU executor cannot extend a chain");

    // Residency is the point. Two keyers, two branch grades and two
    // blends are all chain dispatches, so the frame is still read back
    // exactly once and only the Source takes the per-node CPU path.
    // Before this, the second shape forced flushes=2, starts=2, cpu=3.
    eprintln!("two secondaries: {gpu_stats:?}");
    assert!(
        gpu_stats.extends >= 6,
        "both secondaries should ride one chain; stats {gpu_stats:?}",
    );
    assert_eq!(
        gpu_stats.flushes, 1,
        "one readback for two secondaries, at the end: {gpu_stats:?}",
    );
    assert!(
        gpu_stats.cpu_nodes <= 1,
        "only the Source should touch the per-node CPU path: {gpu_stats:?}",
    );

    // And the picture, against the separate CPU implementations of both
    // keys and both blends.
    assert_eq!(cpu.len(), gpu.len());
    let worst = worst_diff(&cpu, &gpu);
    eprintln!("two secondaries CPU vs GPU: worst {worst:e} linear");
    assert!(worst < 0.001, "CPU and GPU disagree by {worst}");

    // Both secondaries must be doing something, or the comparison above
    // would be two matching no-ops. Dialling only the *second* one out
    // has to change the picture — which is also what catches the second
    // matte being stale, swapped for the first, or dropped.
    let (second_off, _) = run(&two_secondary_graph(Some(0.0)), true);
    let changed = worst_diff(&second_off, &gpu);
    assert!(
        changed > 0.01,
        "the second secondary changed nothing; its matte may be stale or dropped ({changed})",
    );
}

/// A qualifier's matte refinement (clean black and white, shrink/grow,
/// blur) is applied in a recognised secondary too (#1013). The fused
/// GPU path keyed with the window alone and dropped the refinement, so
/// the same grade selected a harder, unshrunk matte on the GPU.
#[test]
fn a_refined_qualifier_in_a_secondary_matches_the_cpu() {
    let refined = |graph: &mut NodeGraph| {
        for n in &mut graph.nodes {
            if let Some(q) = n.params.qualifier.as_mut() {
                q.clean_black = 0.15;
                q.clean_white = 0.15;
                q.shrink_grow = 2.0;
                q.blur = 3.0;
            }
        }
    };
    let mut graph = secondary_graph(Some(1.0));
    refined(&mut graph);
    let (cpu, _) = run(&graph, false);
    let (gpu, stats) = run(&graph, true);
    if stats.extends == 0 && stats.flushes == 0 {
        eprintln!("skipping: no usable GPU");
        return;
    }
    let worst = |a: &[f32], b: &[f32]| {
        a.chunks_exact(4)
            .zip(b.chunks_exact(4))
            .map(|(c, g)| (0..3).map(|i| (c[i] - g[i]).abs()).fold(0.0f32, f32::max))
            .fold(0.0f32, f32::max)
    };
    // The refinement has to change the picture, or the comparison below
    // would pass with it dropped on both sides.
    let (unrefined, _) = run(&secondary_graph(Some(1.0)), false);
    assert!(worst(&cpu, &unrefined) > 0.01, "the refinement changed nothing ({})", worst(&cpu, &unrefined));
    let off = worst(&cpu, &gpu);
    assert!(off < 0.001, "a refined secondary differs between CPU and GPU by {off}");
}

/// A qualifier's matte denoise (#901) alone is a refinement: the fused
/// GPU path, which keys the window alone, must hand it to the CPU's
/// finesse as it does the others, or the GPU keeps the matte's specks.
#[test]
fn a_denoised_qualifier_in_a_secondary_matches_the_cpu() {
    let mut graph = secondary_graph(Some(1.0));
    for n in &mut graph.nodes {
        if let Some(q) = n.params.qualifier.as_mut() {
            q.denoise = 3.0;
        }
    }
    // The frame with grey specks scattered through it, which the key
    // leaves out: a smooth frame's matte has no noise to take out.
    let mut speckled = frame();
    for (i, p) in speckled.chunks_exact_mut(4).enumerate() {
        if (i.wrapping_mul(2_654_435_761) >> 7) % 23 == 0 {
            p[..3].fill(0.5);
        }
    }
    let run_on = |graph: &NodeGraph, gpu: bool| {
        let mut exec = if gpu { GraphExecutor::new() } else { GraphExecutor::new_cpu_only() };
        let out = exec.execute_linear(&speckled, graph, W, H, 0.0, 0, None, None).expect("graph runs").0;
        (out, exec.last_run_chain_stats())
    };
    let (cpu, _) = run_on(&graph, false);
    let (gpu, stats) = run_on(&graph, true);
    if stats.extends == 0 && stats.flushes == 0 {
        eprintln!("skipping: no usable GPU");
        return;
    }
    let worst = |a: &[f32], b: &[f32]| {
        a.chunks_exact(4)
            .zip(b.chunks_exact(4))
            .map(|(c, g)| (0..3).map(|i| (c[i] - g[i]).abs()).fold(0.0f32, f32::max))
            .fold(0.0f32, f32::max)
    };
    let (unrefined, _) = run_on(&secondary_graph(Some(1.0)), false);
    assert!(worst(&cpu, &unrefined) > 0.01, "the denoise changed nothing ({})", worst(&cpu, &unrefined));
    let off = worst(&cpu, &gpu);
    assert!(off < 0.001, "a denoised secondary differs between CPU and GPU by {off}");
}

/// The same with the refinement keyframed rather than set: a blur that
/// is 0 still but keyed up to 8 px is refined on both paths (#1013).
#[test]
fn a_keyframed_refinement_in_a_secondary_matches_the_cpu() {
    let key = |time_secs, value| klipp_core::Keyframe {
        time_secs, value, interpolation: Default::default(), tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
        handles: Default::default(),
    };
    let mut graph = secondary_graph(Some(1.0));
    for n in &mut graph.nodes {
        if n.kind == NodeKind::Qualifier {
            n.keyframes.tracks.push(klipp_nodes::params::NodeKeyframeTrack {
                param_name: "qual_blur".into(),
                keyframes: vec![key(0.0, 8.0), key(1.0, 8.0)],
            });
        }
    }
    let (cpu, _) = run(&graph, false);
    let (gpu, stats) = run(&graph, true);
    if stats.extends == 0 && stats.flushes == 0 {
        eprintln!("skipping: no usable GPU");
        return;
    }
    let worst = |a: &[f32], b: &[f32]| {
        a.chunks_exact(4)
            .zip(b.chunks_exact(4))
            .map(|(c, g)| (0..3).map(|i| (c[i] - g[i]).abs()).fold(0.0f32, f32::max))
            .fold(0.0f32, f32::max)
    };
    let (unrefined, _) = run(&secondary_graph(Some(1.0)), false);
    assert!(worst(&cpu, &unrefined) > 0.005, "the keyed blur changed nothing ({})", worst(&cpu, &unrefined));
    let off = worst(&cpu, &gpu);
    assert!(off < 0.001, "a keyed refinement differs between CPU and GPU by {off}");
}

/// The shape a lifted grade gives `n` gated corrections (M3 step 17b):
/// each correction's port 1 is a qualifier keying the clip's frame, not
/// the correction's own input, so a later gate's matte is keyed from the
/// source while earlier gates' pictures are still in flight. Gives the
/// graph and the keys, in gate order.
fn gates_keyed_off_the_frame(n: usize) -> (NodeGraph, Vec<NodeId>) {
    let (src, out) = (NodeId::new_v4(), NodeId::new_v4());
    let windows = [window(), klipp_core::QualifierSettings { enabled: true, hue_center: 200.0, hue_width: 60.0, hue_soft: 15.0, sat_low: 0.05, sat_high: 1.0, sat_soft: 0.05, lum_low: 0.05, lum_high: 1.0, lum_soft: 0.05, ..Default::default() }, klipp_core::QualifierSettings { enabled: true, hue_center: 100.0, hue_width: 80.0, hue_soft: 20.0, sat_low: 0.0, sat_high: 1.0, sat_soft: 0.05, lum_low: 0.0, lum_high: 1.0, lum_soft: 0.05, ..Default::default() }];
    let grades = [NodeParams { temperature: Some(-40.0), ..Default::default() }, NodeParams { saturation: Some(0.4), ..Default::default() }, NodeParams { contrast: Some(0.3), ..Default::default() }];
    let mut nodes = vec![node(src, NodeKind::Source, Default::default())];
    let mut edges = Vec::new();
    let (mut keys, mut prev) = (Vec::new(), src);
    for g in 0..n {
        let (c, q) = (NodeId::new_v4(), NodeId::new_v4());
        nodes.push(node(q, NodeKind::Qualifier, NodeParams { qualifier: Some(windows[g % 3].clone()), ..Default::default() }));
        nodes.push(node(c, NodeKind::Corrector, grades[g % 3].clone()));
        edges.push(Edge { from_node: prev, from_port: 0, to_node: c, to_port: 0 });
        edges.push(Edge { from_node: src, from_port: 0, to_node: q, to_port: 0 });
        edges.push(Edge { from_node: q, from_port: 0, to_node: c, to_port: 1 });
        keys.push(q);
        prev = c;
    }
    nodes.push(node(out, NodeKind::Output, Default::default()));
    edges.push(Edge { from_node: prev, from_port: 0, to_node: out, to_port: 0 });
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 160.0, y: 0.0 })).collect();
    (NodeGraph { nodes, edges, output_node_id: out, node_positions, working_space_override: None, frames: Vec::new(), sticky_notes: Vec::new() }, keys)
}

/// `graph` on the CPU and the GPU: the GPU's picture, its counters, and
/// its worst difference from the CPU's. `None` without a usable GPU.
fn against_the_cpu(graph: &NodeGraph, matte_tap: Option<NodeId>) -> Option<(klipp_nodes::executor::ChainStats, f32)> {
    let mut cpu = GraphExecutor::new_cpu_only();
    let want = cpu.execute_linear(&frame(), graph, W, H, 0.0, 0, matte_tap, None).expect("graph runs").0;
    let mut gpu = GraphExecutor::new();
    gpu.gpu_pipeline_or_init()?;
    let got = gpu.execute_linear(&frame(), graph, W, H, 0.0, 0, matte_tap, None).expect("graph runs").0;
    let worst = want.iter().zip(&got).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    Some((gpu.last_run_chain_stats(), worst))
}

/// IC11: two and three gated corrections with different mattes, keyed off
/// the frame as a lifted grade keys them, ride one chain: the frame goes up
/// once, every key, correction and gate is a chain step, and it comes back
/// once (on main: two readbacks, five nodes on the CPU). Each keeps its
/// own picture and matte in slots, released after their gate, so the peak
/// stays small, and the picture is the CPU's.
#[test]
fn two_gated_corrections_with_different_mattes_do_not_flush() {
    for n in [2, 3] {
        let (graph, _) = gates_keyed_off_the_frame(n);
        let Some((stats, worst)) = against_the_cpu(&graph, None) else {
            eprintln!("skipping: no usable GPU");
            return;
        };
        eprintln!("{n} gates keyed off the frame: {stats:?}, worst {worst}");
        assert!(worst < 1e-3, "{n} gates: GPU differs from the CPU by {worst}");
        assert_eq!(stats.flushes, 1, "{n} gates: one readback, at the end: {stats:?}");
        assert!(stats.cpu_nodes <= 1, "{n} gates: only the source off the chain: {stats:?}");
        // Each key, gate and correction is a chain step, the first
        // correction starting the chain.
        assert!(stats.extends as usize >= 3 * n - 1, "{n} gates: each key, correction and gate is a chain step: {stats:?}");
        assert!(stats.peak_slots <= 4, "{n} gates: slots go back after their gate: {stats:?}");
    }
}

/// A key that reads a picture no secondary keeps (here a side correction
/// of the frame, drawn by no gate) doesn't make a recognised secondary: a
/// slot would have to hold a picture nothing keeps, or a stale one. That
/// gate runs off the chain, and the picture is still the CPU's.
#[test]
fn a_key_of_a_picture_written_later_falls_back() {
    let (mut graph, keys) = gates_keyed_off_the_frame(2);
    let src = graph.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id;
    let side = node(NodeId::new_v4(), NodeKind::Corrector, NodeParams { contrast: Some(0.5), ..Default::default() });
    graph.edges.push(Edge { from_node: src, from_port: 0, to_node: side.id, to_port: 0 });
    for e in &mut graph.edges {
        if e.to_node == keys[1] {
            e.from_node = side.id;
        }
    }
    graph.nodes.push(side);
    let (_, alone) = against_the_cpu(&gates_keyed_off_the_frame(1).0, None).unwrap_or_default();
    let Some((stats, worst)) = against_the_cpu(&graph, None) else { return };
    eprintln!("a key of a picture nothing keeps: {stats:?} (one gate alone: worst {alone})");
    assert!(worst < 1e-3, "GPU differs from the CPU by {worst}");
    assert!(stats.cpu_nodes >= 2, "the second gate runs off the chain: {stats:?}");
}

/// A key whose matte is looked at (tapped) keeps its pixels on the CPU:
/// its gate reads them there, and the picture is still the CPU's.
#[test]
fn a_tapped_key_falls_back() {
    let (graph, keys) = gates_keyed_off_the_frame(2);
    let Some((stats, worst)) = against_the_cpu(&graph, Some(keys[1])) else { return };
    eprintln!("a tapped key: {stats:?}");
    assert!(worst < 1e-3, "GPU differs from the CPU by {worst}");
}

/// More gates keyed off one frame cost no more slots (M3 step 17b): the
/// frame, the previous gate's picture and one key's matte at a time, each
/// key drawn just before its gate and given back after it.
#[test]
fn peak_slots_does_not_grow_with_n() {
    let mut peaks = Vec::new();
    for n in 2..=5 {
        let (graph, _) = gates_keyed_off_the_frame(n);
        let Some((stats, worst)) = against_the_cpu(&graph, None) else { return };
        assert!(worst < 1e-3, "{n} gates: GPU differs from the CPU by {worst}");
        assert_eq!(stats.flushes, 1, "{n} gates ride one chain: {stats:?}");
        peaks.push(stats.peak_slots);
    }
    eprintln!("peak slots for 2..=5 gates: {peaks:?}");
    assert!(peaks.iter().all(|p| *p == peaks[0]), "the peak grows with the gates: {peaks:?}");
    assert!(peaks[0] <= 3, "the frame, a gate's picture and a matte: {peaks:?}");
}

/// Lead decision 3's check (M3 step 17b): the pool's slots and the
/// chain's other kept buffers never alias. Two gates keyed off the frame,
/// each correction mixing over its own input (its Keep/Mix pair, on the
/// Corrector's own buffer, written and read inside the gate's branch)
/// while the frame, the first gate's picture and a matte sit in slots:
/// the chain carries it all (one readback, the slots held at once), and
/// the picture is the CPU's. (Composites beside a secondary and a mixed
/// Corrector: `composite_gpu_parity`.)
#[test]
fn leased_and_secondary_never_alias() {
    let (mut graph, _) = gates_keyed_off_the_frame(2);
    for n in graph.nodes.iter_mut().filter(|n| n.kind == NodeKind::Corrector) {
        n.params.global_mix = Some(0.6);
    }
    let Some((stats, worst)) = against_the_cpu(&graph, None) else { return };
    eprintln!("gates with mixed correctors: {stats:?}, worst {worst}");
    assert!(worst < 1e-3, "a slot aliased another value: GPU differs from the CPU by {worst}");
    assert_eq!(stats.flushes, 1, "the gates and the mixes ride one chain: {stats:?}");
    assert!(stats.peak_slots >= 3, "the frame, a gate's picture and a matte held at once: {stats:?}");
}

/// A key switched off at this frame by its enabled track (though on in its
/// settings) is bypassed, as any switched-off node is: its gate isn't
/// drawn from a key the GPU keyed anyway, and the picture is the CPU's at
/// a frame where it is off and one where it is on.
#[test]
fn a_key_switched_off_by_its_track_is_bypassed_at_its_gate() {
    // Keyed off the frame, and (rewired to the first correction, which
    // the gates turn into the first gate's picture) off a kept picture the
    // chain has moved past by the time the key's turn comes.
    for off_a_gate in [false, true] {
        let (mut graph, keys) = gates_keyed_off_the_frame(2);
        if off_a_gate {
            let first = graph.edges.iter().find(|e| e.from_node == keys[0] && e.to_port == 1).unwrap().to_node;
            for e in graph.edges.iter_mut().filter(|e| e.to_node == keys[1]) {
                e.from_node = first;
            }
        }
        let key = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Step, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
        if let Some(n) = graph.nodes.iter_mut().find(|n| n.id == keys[1]) {
            n.keyframes.tracks.push(klipp_nodes::params::NodeKeyframeTrack { param_name: klipp_nodes::executor::ENABLED_TRACK.into(), keyframes: vec![key(0.0, 0.0), key(1.0, 1.0)] });
        }
        for t in [0.0, 1.5] {
            let mut cpu = GraphExecutor::new_cpu_only();
            let want = cpu.execute_linear(&frame(), &graph, W, H, t, 0, None, None).expect("graph runs").0;
            let mut gpu = GraphExecutor::new();
            if gpu.gpu_pipeline_or_init().is_none() {
                return;
            }
            let got = gpu.execute_linear(&frame(), &graph, W, H, t, 0, None, None).expect("graph runs").0;
            let worst = want.iter().zip(&got).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            eprintln!("key off until 1 s (off a gate: {off_a_gate}), at {t} s: {:?}, worst {worst}", gpu.last_run_chain_stats());
            assert!(worst < 1e-3, "off a gate {off_a_gate}, at {t} s the GPU differs from the CPU by {worst}");
        }
    }
}

