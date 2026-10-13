//! A scene-referred clip's display step as a plan node (phase 2b M2, step
//! 11c): `SceneToDisplay` on the GPU chain draws what
//! `klipp_color::hdr::SceneToDisplay::apply` draws on the CPU, for every
//! display branch, tone-mapped and as display light, signed the way the
//! CPU is: a colour outside the working gamut arrives negative and stays
//! negative, which the viewer's monitor encode (it clamps first) would
//! not keep.
//!
//! A missing GPU leaves the GPU half unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use klipp_color::hdr::{HdrTransfer, SceneToDisplay};
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 32;
const H: u32 = 16;

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// Scene light from below black to well over the top of each curve, the
/// channels apart, and a strip of out-of-gamut colours with a negative
/// channel.
fn frame() -> Vec<f32> {
    (0..W * H)
        .flat_map(|i| {
            let (x, y) = ((i % W) as f32 / (W - 1) as f32, (i / W) as f32 / (H - 1) as f32);
            let v = 60.0 * y * y;
            if i % W < 4 {
                [-0.05 - 0.2 * y, v * 0.5, v, 1.0]
            } else {
                [v, v * (0.4 + 0.6 * x), v * (1.0 - 0.7 * x), 0.5 + 0.5 * x]
            }
        })
        .collect()
}

fn graph(step: SceneToDisplay) -> NodeGraph {
    let node = |kind, params| Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] };
    let s = node(NodeKind::Source, NodeParams::default());
    let d = node(NodeKind::SceneToDisplay, NodeParams { scene_to_display: Some(step), ..Default::default() });
    let o = node(NodeKind::Output, NodeParams::default());
    let edges = vec![Edge { from_node: s.id, from_port: 0, to_node: d.id, to_port: 0 }, Edge { from_node: d.id, from_port: 0, to_node: o.id, to_port: 0 }];
    NodeGraph { output_node_id: o.id, nodes: vec![s, d, o], edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

fn steps() -> Vec<(String, SceneToDisplay)> {
    let luma = klipp_color::primaries::luminance_weights(klipp_core::WorkingColorspace::default());
    let transfers = [("PQ", HdrTransfer::Pq), ("HLG", HdrTransfer::Hlg), ("S-Log3", HdrTransfer::Log(klipp_core::ColorTransfer::SLog3))];
    transfers
        .iter()
        .flat_map(|&(name, transfer)| [None, Some(1000.0)].map(|peak_nits| (format!("{name}, peak {peak_nits:?}"), SceneToDisplay { transfer, peak_nits, luma })))
        .collect()
}

/// The CPU executor runs the step exactly as the legacy route does, and
/// the GPU chain within a part in ten thousand of it, on the chain.
#[test]
fn the_display_step_on_the_chain_draws_the_cpu_step() {
    let mut cpu = GraphExecutor::new_cpu_only();
    let mut gpu = GraphExecutor::new();
    // A scene-referred clip's run, as the renderer says it is (#1867).
    for ex in [&mut cpu, &mut gpu] {
        ex.set_source_domain(klipp_nodes::domains::PictureDomain::Scene);
    }
    let on_gpu = gpu.gpu_pipeline_or_init().is_some();
    assert!(on_gpu || !required(), "required, but there is no GPU");
    let input = frame();
    for (case, step) in steps() {
        let mut want = input.clone();
        step.apply(&mut want);
        let got = cpu.execute_linear(&input, &graph(step), W, H, 0.0, 0, None, None).unwrap().0;
        assert!(got.iter().map(|v| v.to_bits()).eq(want.iter().map(|v| v.to_bits())), "cpu: {case}");
        if !on_gpu {
            continue;
        }
        let got = gpu.execute_linear(&input, &graph(step), W, H, 0.0, 0, None, None).unwrap().0;
        let stats = gpu.last_run_chain_stats();
        // The source is CPU pixels, so the Source node is the one CPU
        // node; the step is the chain's one dispatch, flushed once.
        assert_eq!((stats.cpu_nodes, stats.flushes, stats.submits), (1, 1, 1), "gpu: {case}: on the chain ({stats:?})");
        let worst = got.iter().zip(&want).map(|(a, b)| (a - b).abs() / b.abs().max(1.0)).fold(0.0f32, f32::max);
        eprintln!("gpu: {case}: worst {worst}");
        assert!(worst <= 1e-4, "gpu: {case}: worst {worst}");
        // Signed like the CPU: what the CPU leaves negative, the chain does.
        let negative = |v: &[f32]| v.iter().enumerate().filter(|(i, x)| i % 4 != 3 && **x < 0.0).count();
        assert_eq!(negative(&got), negative(&want), "gpu: {case}: negative channels kept");
    }
    if !on_gpu {
        eprintln!("UNVERIFIED: no GPU");
    }
}

/// The node isn't a passthrough in disguise: each branch changes the
/// picture, the SDR tone map differs between a PQ or HLG source and a
/// camera-log one (their exposures differ), each display-light branch
/// differs from every other step, and some branch keeps a negative
/// channel, so the signed check above bites.
#[test]
fn every_branch_changes_the_picture() {
    let input = frame();
    let out: Vec<(String, Vec<f32>)> = steps()
        .into_iter()
        .map(|(case, step)| {
            let mut out = input.clone();
            step.apply(&mut out);
            (case, out)
        })
        .collect();
    for (case, o) in &out {
        assert!(*o != input, "{case}: changed the picture");
    }
    let find = |name: &str| &out.iter().find(|(c, _)| c == name).unwrap().1;
    // PQ and HLG tone-map alike for an SDR picture: one exposure, 0.18.
    assert_eq!(find("PQ, peak None"), find("HLG, peak None"));
    assert_ne!(find("PQ, peak None"), find("S-Log3, peak None"));
    for (case, o) in out.iter().filter(|(c, _)| c.ends_with("Some(1000.0)")) {
        assert_eq!(out.iter().filter(|(_, other)| other == o).count(), 1, "{case}: differs from every other step");
    }
    let negative = out.iter().any(|(_, o)| o.iter().enumerate().any(|(i, x)| i % 4 != 3 && *x < 0.0));
    assert!(negative, "some branch keeps a negative channel");
}
