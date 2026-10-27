//! An effect node's stabilisation and rolling-shutter correction warp by
//! the frame's motion from its analysis (node-graph M2 step 12, M4 step
//! 9): on the CPU arm and on the executor's own GPU pipeline alike, and
//! not at all without one. A missing GPU leaves the GPU half unverified;
//! with `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use klipp_effects::homography::Homography;
use klipp_effects::node_analysis::NodeAnalysis;
use klipp_effects::BuiltinEffect as E;
use klipp_nodes::frame_context::{Analysis, AnalysisBlob, AnalysisInputs, FrameContext, NoSources, PluginInstances, NODE_ANALYSIS_SCHEMA};
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

/// A correction as stabilisation makes them: a small turn and a shift,
/// in the frame's units (centred, its height 1).
fn turned() -> Homography {
    let (s, c) = 2.5f64.to_radians().sin_cos();
    Homography { h: [[c, -s, 0.04], [s, c, -0.03], [0.0, 0.0, 1.0]] }
}

/// Every effect's analysis, whatever node asks: a stabilisation's
/// corrections and the file's rolling-shutter motion, each the same at
/// every frame. Empty, none.
struct Given(Option<(AnalysisBlob, AnalysisBlob)>);

impl Given {
    fn analysed() -> Self {
        use klipp_effects::rolling_shutter::RollingShutterData;
        use klipp_effects::stabilization::SmoothedTrajectory;
        let blob = |a: NodeAnalysis| AnalysisBlob { schema: NODE_ANALYSIS_SCHEMA, data: std::sync::Arc::new(a) };
        let stab = NodeAnalysis::Stabilisation {
            smoothed: SmoothedTrajectory { smoothed: Vec::new(), corrections: vec![turned(); 48], smoothing_window_secs: 1.0, raw_jitter_px: 1.0, smoothed_jitter_px: 0.1 },
            fps: 24.0,
        };
        let step = |dx: f64, dy: f64| Homography { h: [[1.0, 0.02, dx], [0.0, 1.0, dy], [0.0, 0.0, 1.0]] };
        let rsc = NodeAnalysis::RollingShutter(RollingShutterData { motion: vec![(step(-7.5, -1.25), step(7.5, 1.25)); 48], fps: 24.0 });
        Self(Some((blob(stab), blob(rsc))))
    }
}

impl AnalysisInputs for Given {
    fn lookup(&self, _: NodeId, _: u32) -> Analysis<'_> {
        Analysis::Absent
    }
    fn lookup_effect(&self, _: NodeId, effect: &E, _: u32) -> Analysis<'_> {
        match (&self.0, effect) {
            (Some((stab, _)), E::VideoStabilization { .. }) => Analysis::Ready(stab),
            (Some((_, rsc)), E::RollingShutterCorrection { .. }) => Analysis::Ready(rsc),
            _ => Analysis::Absent,
        }
    }
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

fn run(ex: &mut GraphExecutor, g: &NodeGraph, analysis: &Given) -> Vec<f32> {
    let (mut sources, mut instances) = (NoSources, PluginInstances::default());
    let mut ctx = FrameContext {
        clip: klipp_core::ClipId::new(),
        frame: 12,
        source: (klipp_core::MediaId::nil(), 0.0),
        clip_time_secs: 0.5,
        source_secs: 0.5,
        source_pts: None,
        frame_number: 12,
        fps: 24.0,
        sources: &mut sources,
        analysis,
        instances: &mut instances,
        ofx: None,
        source_at: &|t| t,
    };
    ex.execute_linear_with_context(&frame(), g, W, H, 0.5, 12, None, None, None, &mut ctx).unwrap().0
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
fn an_analysis_reaches_both_arms() {
    let (analysed, none) = (Given::analysed(), Given(None));
    let cases = [
        ("stabilise", vec![E::VideoStabilization { smoothing: 0.5, crop_ratio: 0.05, zoom: true }]),
        ("rolling shutter", vec![E::RollingShutterCorrection { strength: 0.8, readout_ratio: 0.4 }]),
    ];
    for (name, fx) in cases {
        let g = graph(fx);
        let mut cpu = GraphExecutor::new_cpu_only();
        let still = run(&mut cpu, &g, &none);
        let warped = run(&mut cpu, &g, &analysed);
        assert!(mean_diff(&warped, &still) > 0.01, "{name}: the CPU arm warps by its analysis");
        assert_eq!(run(&mut cpu, &g, &none), still, "{name}: without one, the CPU arm passes the frame through");
        if let Some(mut ex) = gpu() {
            let on_gpu = run(&mut ex, &g, &analysed);
            assert!(mean_diff(&on_gpu, &warped) < 1.0 / 255.0, "{name}: the GPU warps as the CPU does ({})", mean_diff(&on_gpu, &warped));
            assert!(mean_diff(&run(&mut ex, &g, &none), &still) < 1.0 / 255.0, "{name}: without one, the GPU passes the frame through");
        }
    }
}
