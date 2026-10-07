//! An effect node's stabilisation and rolling-shutter correction warp by
//! the frame's motion the renderer hands the executor (node-graph M2
//! step 12): on the CPU arm and on the executor's own GPU pipeline alike,
//! and not at all once the motion is cleared. A missing GPU leaves the GPU
//! half unverified; with `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use klipp_effects::rolling_shutter::RowTransform;
use klipp_effects::{BuiltinEffect as E, MotionData};
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 96;
const H: u32 = 54;

/// Edges and a gradient, so a warp moves something.
fn frame() -> Vec<f32> {
    (0..W * H)
        .flat_map(|i| {
            let (x, y) = (i % W, i / W);
            let block = if ((x / 8) + (y / 8)) % 2 == 0 { 0.9 } else { 0.2 };
            [block * x as f32 / W as f32, block, block * y as f32 / H as f32, 1.0]
        })
        .collect()
}

/// A correction as stabilisation makes them: a small turn and a shift.
fn homography() -> [f32; 12] {
    let (s, c) = 2.5f32.to_radians().sin_cos();
    [c, -s, 6.3, s, c, -4.7, 0.00004, -0.00003, 1.0, 0.0, 0.0, 0.0]
}

/// Rows sheared and shifted progressively down the frame.
fn rows() -> Vec<RowTransform> {
    (0..H)
        .map(|y| {
            let t = y as f32 / H as f32;
            RowTransform { a: 1.0, b: 0.02 * t, tx: 7.5 * t - 2.0, c: 0.0, d: 1.0, ty: 1.25 * t, _pad0: 0.0, _pad1: 0.0 }
        })
        .collect()
}

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
}

/// Source -> Effect(`fx`) -> Output.
fn graph(fx: Vec<E>) -> NodeGraph {
    let s = node(NodeKind::Source, NodeParams::default());
    let e = node(NodeKind::Effect, NodeParams { effects: fx, ..Default::default() });
    let o = node(NodeKind::Output, NodeParams::default());
    let link = |a: &Node, b: &Node| Edge { from_node: a.id, from_port: 0, to_node: b.id, to_port: 0 };
    NodeGraph {
        output_node_id: o.id,
        edges: vec![link(&s, &e), link(&e, &o)],
        nodes: vec![s, e, o],
        node_positions: Default::default(),
        working_space_override: None,
        frames: vec![],
        sticky_notes: vec![],
    }
}

fn run(ex: &mut GraphExecutor, g: &NodeGraph) -> Vec<f32> {
    ex.execute_linear(&frame(), g, W, H, 0.5, 12, None, None).unwrap().0
}

fn mean_diff(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f32>() / a.len() as f32
}

fn gpu() -> Option<GraphExecutor> {
    let mut ex = GraphExecutor::new();
    let ok = ex.gpu_pipeline_or_init().is_some();
    let required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1");
    assert!(ok || !required, "required, but there is no GPU");
    if !ok {
        eprintln!("UNVERIFIED: no GPU");
    }
    ok.then_some(ex)
}

#[test]
fn set_motion_reaches_both_arms() {
    let motion = MotionData { stabilization: Some(homography()), rsc_rows: Some(rows()) };
    let cases = [
        ("stabilise", vec![E::VideoStabilization { smoothing: 0.5, crop_ratio: 0.05, zoom: true }]),
        ("rolling shutter", vec![E::RollingShutterCorrection { strength: 0.8, readout_ratio: 0.4 }]),
    ];
    for (name, fx) in cases {
        let g = graph(fx);
        let mut cpu = GraphExecutor::new_cpu_only();
        let still = run(&mut cpu, &g);
        cpu.set_motion(&motion);
        let warped = run(&mut cpu, &g);
        assert!(mean_diff(&warped, &still) > 0.01, "{name}: the CPU arm warps by the motion it was given");
        cpu.set_motion(&MotionData::default());
        assert_eq!(run(&mut cpu, &g), still, "{name}: cleared, the CPU arm passes the frame through");
        if let Some(mut ex) = gpu() {
            // Set before the run that builds the pipeline's buffers, and again after.
            ex.set_motion(&motion);
            let on_gpu = run(&mut ex, &g);
            assert!(mean_diff(&on_gpu, &warped) < 1.0 / 255.0, "{name}: the GPU warps as the CPU does ({})", mean_diff(&on_gpu, &warped));
            ex.set_motion(&MotionData::default());
            assert!(mean_diff(&run(&mut ex, &g), &still) < 1.0 / 255.0, "{name}: cleared, the GPU passes the frame through");
        }
    }
}

/// Motion set before the executor's GPU pipeline exists reaches it when
/// the pipeline is built.
#[test]
fn motion_set_before_the_pipeline_is_built_reaches_it() {
    let Some(_) = gpu() else { return };
    let g = graph(vec![E::VideoStabilization { smoothing: 0.5, crop_ratio: 0.05, zoom: true }]);
    let motion = MotionData { stabilization: Some(homography()), rsc_rows: None };
    let mut cpu = GraphExecutor::new_cpu_only();
    cpu.set_motion(&motion);
    let want = run(&mut cpu, &g);
    let mut ex = GraphExecutor::new();
    ex.set_motion(&motion);
    let got = run(&mut ex, &g);
    assert!(mean_diff(&got, &want) < 1.0 / 255.0, "{}", mean_diff(&got, &want));
}
