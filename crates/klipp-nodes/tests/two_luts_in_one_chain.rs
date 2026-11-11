//! Two 3D LUT nodes in one graph, each with its own table, run on the
//! GPU as one chain. A table goes up by a queue write, which lands before
//! the chain's encoder runs whatever was recorded ahead of it, so two
//! lookups sharing one table buffer would both read the last one written.
//! Each must read its own: the GPU's picture is the CPU's.

use klipp_core::WorkingColorspace;
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId, NodePos};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 128;
const H: u32 = 64;

fn node(id: NodeId, kind: NodeKind, params: NodeParams) -> Node {
    Node { id, kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() }
}

/// Source, then each node of `chain` in turn, then Output.
fn graph(chain: Vec<(NodeKind, NodeParams)>) -> NodeGraph {
    let mut nodes = vec![node(NodeId::new_v4(), NodeKind::Source, Default::default())];
    nodes.extend(chain.into_iter().map(|(k, p)| node(NodeId::new_v4(), k, p)));
    nodes.push(node(NodeId::new_v4(), NodeKind::Output, Default::default()));
    let edges = nodes.windows(2).map(|w| Edge { from_node: w[0].id, from_port: 0, to_node: w[1].id, to_port: 0 }).collect();
    let node_positions = nodes.iter().enumerate().map(|(i, m)| (m.id, NodePos { x: i as f32 * 200.0, y: 0.0 })).collect();
    NodeGraph { output_node_id: nodes.last().unwrap().id, nodes, edges, node_positions, working_space_override: None, frames: Vec::new(), sticky_notes: Vec::new() }
}

/// A ramp from black to white across, hue turning down the frame.
fn frame() -> Vec<f32> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let (t, u) = (x as f32 / (W - 1) as f32, y as f32 / (H - 1) as f32);
            v.extend_from_slice(&[t * t, t * (0.6 + 0.4 * u), t * (1.0 - 0.5 * u), 1.0]);
        }
    }
    v
}

fn lut(file: &str) -> NodeParams {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/luts").join(file);
    assert!(path.exists(), "{path:?}");
    NodeParams { lut_path: Some(path), lut_opacity: Some(1.0), ..Default::default() }
}

fn run(g: &NodeGraph, gpu: bool) -> (Vec<f32>, klipp_nodes::executor::ChainStats) {
    let mut exec = if gpu { GraphExecutor::new() } else { GraphExecutor::new_cpu_only() };
    exec.set_working_colorspace(WorkingColorspace::Rec709Linear);
    let out = exec.execute_linear(&frame(), g, W, H, 0.0, 0, None, None).expect("graph runs").0;
    (out, exec.last_run_chain_stats())
}

/// The worst difference in display levels (of 255).
fn worst_levels(a: &[f32], b: &[f32]) -> f32 {
    let enc = |v: f32| klipp_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0));
    a.chunks_exact(4).zip(b.chunks_exact(4)).flat_map(|(p, q)| (0..3).map(move |i| (enc(p[i]) - enc(q[i])).abs())).fold(0.0f32, f32::max) * 255.0
}

/// Frame after frame, a graph's tables go up once each: a frame does
/// not send them again.
#[test]
fn a_lut_goes_up_once_not_every_frame() {
    let both = graph(vec![(NodeKind::Lut3D, lut("fuji-xtrans/fuji_xtrans_iii_classic_chrome.cube")), (NodeKind::Lut3D, lut("fuji-xtrans/fuji_xtrans_iii_acros.cube"))]);
    let mut exec = GraphExecutor::new();
    exec.set_working_colorspace(WorkingColorspace::Rec709Linear);
    let input = frame();
    for n in 0..5 {
        exec.execute_linear(&input, &both, W, H, n as f64 / 25.0, n, None, None).expect("graph runs");
    }
    let Some(gpu) = exec.gpu_pipeline_mut() else {
        assert!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref() != Ok("1"), "no usable GPU");
        eprintln!("skipping: no usable GPU");
        return;
    };
    assert_eq!(gpu.chain_lut_uploads(), 2, "two tables, each uploaded once over five frames");
}

#[test]
fn two_luts_in_one_chain_each_read_their_own_table() {
    let (a, b) = (lut("fuji-xtrans/fuji_xtrans_iii_classic_chrome.cube"), lut("fuji-xtrans/fuji_xtrans_iii_acros.cube"));
    let both = graph(vec![(NodeKind::Lut3D, a.clone()), (NodeKind::Lut3D, b.clone())]);
    let (cpu, _) = run(&both, false);
    let (gpu, stats) = run(&both, true);
    if stats.extends == 0 && stats.flushes == 0 {
        if std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref() == Ok("1") {
            panic!("no usable GPU");
        }
        eprintln!("skipping: no usable GPU");
        return;
    }
    // The two looks differ, or the test could not tell them apart.
    let (only_b, _) = run(&graph(vec![(NodeKind::Lut3D, b.clone()), (NodeKind::Lut3D, b.clone())]), false);
    assert!(worst_levels(&cpu, &only_b) > 4.0, "the two tables give the same picture: the test can't see a shared one");
    let w = worst_levels(&cpu, &gpu);
    assert!(w < 1.0, "the GPU's two LUTs differ from the CPU's by {w:.2} display levels: one read the other's table");
}

/// More LUTs in one chain than the GPU keeps tables for (8): the first
/// table is let go while the chain that reads it is still being recorded.
/// The picture is still the CPU's.
#[test]
fn ten_luts_in_one_chain_match_the_cpu() {
    let files = [
        "cinematic/D-CINELIKE -0_3 BASE.cube",
        "cinematic/D-CINELIKE -0_3 V2.cube",
        "cinematic/NORMAL -1_0 BASE.cube",
        "cinematic/NORMAL -1_0 SCENE 01.cube",
        "cinematic/NORMAL -1_0 SCENE 02.cube",
        "cinematic/NORMAL -1_0 SKIN.cube",
        "fuji-xtrans/fuji_xtrans_iii_classic_chrome.cube",
        "fuji-xtrans/fuji_xtrans_iii_acros.cube",
        "film-bw/agfa_apx_100.cube",
        "film-bw/fuji_neopan_1600.cube",
    ];
    let mut chain: Vec<(NodeKind, NodeParams)> = files.iter().map(|f| (NodeKind::Lut3D, NodeParams { lut_opacity: Some(0.35), ..lut(f) })).collect();
    chain.truncate(10);
    let g = graph(chain);
    let (cpu, _) = run(&g, false);
    let (gpu, stats) = run(&g, true);
    if stats.extends == 0 && stats.flushes == 0 {
        assert!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref() != Ok("1"), "no usable GPU");
        return;
    }
    // All on the GPU, in one chain: the source, then nine extends.
    assert!(stats.extends >= 9 && stats.cpu_nodes <= 1, "not one GPU chain: {stats:?}");
    let w = worst_levels(&cpu, &gpu);
    assert!(w < 1.5, "ten LUTs in one chain: the GPU differs from the CPU by {w:.2} display levels");
}

/// The same for LUT effects, of which the GPU keeps four tables: six in
/// one effect node, each its own table.
#[test]
fn six_lut_effects_in_one_chain_match_the_cpu() {
    use klipp_effects::colour_transform::{LutTable, Prepared};
    let table = |k: f32| {
        let n = 9;
        let mut data = Vec::with_capacity(n * n * n * 3);
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    let [r, g, b] = [r, g, b].map(|i| i as f32 / (n - 1) as f32);
                    data.extend_from_slice(&[r.powf(k), g.powf(1.0 / k), b * k.min(1.0)]);
                }
            }
        }
        LutTable { size: n, data, pre: None, post: None }
    };
    let effects: Vec<klipp_effects::BuiltinEffect> = [0.7f32, 0.8, 0.9, 1.1, 1.25, 1.4]
        .iter()
        .map(|k| klipp_effects::BuiltinEffect::Lut3D { path: format!("look-{k}.cube"), opacity: 1.0, tetrahedral: true, prepared: Some(Prepared(std::sync::Arc::new(table(*k)))) })
        .collect();
    let g = graph(vec![(NodeKind::Effect, NodeParams { effects, ..Default::default() })]);
    let (cpu, _) = run(&g, false);
    let (gpu, stats) = run(&g, true);
    if stats.extends == 0 && stats.flushes == 0 {
        assert!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref() != Ok("1"), "no usable GPU");
        return;
    }
    // Recorded into the chain, not run as breaks of their own.
    assert_eq!(stats.effect_breaks, 0, "the LUT effects broke the chain: {stats:?}");
    let w = worst_levels(&cpu, &gpu);
    assert!(w < 1.5, "six LUT effects in one chain: the GPU differs from the CPU by {w:.2} display levels");
}
