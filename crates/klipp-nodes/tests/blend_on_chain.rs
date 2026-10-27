//! A slowed clip's frame blend on the executor's GPU chain (#2854): a
//! lone frame-blend node draws on the chain toward the media's next
//! frame, cross-faded or followed by its motion, and the picture is the
//! one the node draws off the chain, from the same frames, within a level
//! of 8-bit sRGB. So is one whose frame shown was the one before the time
//! (the next frame starts the blend). A missing GPU leaves this
//! unverified; with `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use std::sync::Arc;

use klipp_core::{ClipId, MediaId};
use klipp_effects::BuiltinEffect as E;
use klipp_nodes::frame_context::{BoundedSources, EncodedFrame, FrameContext, LinearFrame, Neighbour, NoAnalysis, PluginInstances, SourceProvider};
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 160;
const H: u32 = 96;
const FPS: f64 = 25.0;

/// A smooth texture, moving two pixels across a media frame: what the
/// media shows at `secs`.
fn shown(secs: f64) -> Vec<f32> {
    let shift = secs * FPS * 2.0;
    (0..W * H)
        .flat_map(|i| {
            let (x, y) = ((i % W) as f64 - shift, (i / W) as f64);
            let v = 0.45 + 0.2 * (x * 0.21).sin() * (y * 0.17).cos() + 0.15 * (x * 0.07 + y * 0.11).sin();
            [v as f32, (v * 0.8) as f32, (1.0 - v) as f32 * 0.6, 1.0]
        })
        .collect()
}

/// A media of ten seconds, a frame every 1/25 s from 0.
struct Media;

impl SourceProvider for Media {
    fn bounds(&self) -> (f64, f64) {
        (0.0, 10.0)
    }
    fn frame_secs(&self) -> Option<f64> {
        Some(1.0 / FPS)
    }
    fn encoded(&mut self, source_secs: f64) -> Neighbour<EncodedFrame> {
        Neighbour::Frame(EncodedFrame { rgba8: vec![0; (W * H * 4) as usize], width: W, height: H, source_secs })
    }
    fn linear(&mut self, source_secs: f64) -> Neighbour<LinearFrame> {
        Neighbour::Frame(LinearFrame { rgba: Arc::new(shown(source_secs)), width: W, height: H, source_secs, pts: Some(source_secs) })
    }
    fn next(&mut self, source_secs: f64) -> Neighbour<LinearFrame> {
        let pts = ((source_secs * FPS).round() + 1.0) / FPS;
        Neighbour::Frame(LinearFrame { rgba: Arc::new(shown(pts)), width: W, height: H, source_secs: pts, pts: Some(pts) })
    }
}

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
}

/// Source, the frame blend in `blend_mode` (none without one), a grade,
/// Output.
fn graph(blend_mode: Option<u32>) -> NodeGraph {
    let mut nodes = vec![node(NodeKind::Source, NodeParams::default())];
    nodes.extend(blend_mode.map(|blend_mode| node(NodeKind::Effect, NodeParams { effects: vec![E::SpeedRamp { speed: 1.0, blend_mode }], ..Default::default() })));
    nodes.push(node(NodeKind::Exposure, NodeParams { exposure: Some(0.3), ..Default::default() }));
    nodes.push(node(NodeKind::Output, NodeParams::default()));
    let edges = nodes.windows(2).map(|w| Edge { from_node: w[0].id, from_port: 0, to_node: w[1].id, to_port: 0 }).collect();
    NodeGraph { output_node_id: nodes.last().unwrap().id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

/// The picture drawn at source time `at` from the frame shown at `pts`,
/// and the blends drawn on the chain.
fn run(ex: &mut GraphExecutor, g: &NodeGraph, clip: ClipId, at: f64, pts: f64) -> (Vec<f32>, u32) {
    let mut media = Media;
    let mut bounded = BoundedSources::new(&mut media, W, H);
    let mut instances = PluginInstances::default();
    let mut ctx = FrameContext {
        clip,
        frame: 10,
        source: (MediaId::nil(), 0.0),
        clip_time_secs: at,
        source_secs: at,
        source_pts: Some(pts),
        frame_number: 10,
        fps: FPS,
        sources: &mut bounded,
        analysis: &NoAnalysis,
        instances: &mut instances,
        ofx: None,
        source_at: &|t| t,
    };
    let pixels = ex.execute_linear_with_context(&shown(pts), g, W, H, at, 10, None, None, None, &mut ctx).expect("the run").0;
    (pixels, ex.last_blend_on_chain())
}

fn gpu() -> Option<GraphExecutor> {
    let mut ex = GraphExecutor::new();
    let ok = ex.gpu_pipeline_or_init().is_some();
    assert!(ok || std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
    if !ok {
        eprintln!("UNVERIFIED: no GPU");
    }
    ok.then_some(ex)
}

fn srgb8(v: f32) -> i32 {
    (klipp_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as i32
}

/// The worst difference between two pictures in 8-bit sRGB levels.
fn levels(a: &[f32], b: &[f32]) -> i32 {
    assert_eq!(a.len(), b.len());
    a.chunks_exact(4).zip(b.chunks_exact(4)).map(|(p, q)| (0..3).map(|c| (srgb8(p[c]) - srgb8(q[c])).abs()).max().unwrap()).max().unwrap()
}

#[test]
fn a_frame_blend_draws_on_the_chain_as_off_it() {
    let Some(mut on) = gpu() else { return };
    let Some(mut off) = gpu() else { return };
    off.set_blend_on_chain(false);
    let frame = 1.0 / FPS;
    // At 0.4 s a frame falls on 0.4: 0.3 of the way to the next.
    // Shown the frame before (a source that rounds down twice): the next
    // one starts the blend, 0.3 of the way to the one after.
    let cases = [("between two frames", 0.4 + 0.3 * frame, 0.4), ("from the frame before", 0.4 + 0.3 * frame, 0.4 - frame)];
    for (mode, what_mode) in [(1, "cross-faded"), (2, "by flow")] {
        let g = graph(Some(mode));
        let clip = ClipId::new();
        for (what, at, pts) in cases {
            let (chain, drawn_on) = run(&mut on, &g, clip, at, pts);
            let (route, drawn_off) = run(&mut off, &g, clip, at, pts);
            assert_eq!((drawn_on, drawn_off), (1, 0), "{what_mode}, {what}: drawn on the chain, and off it with it turned off");
            let apart = levels(&chain, &route);
            assert!(apart <= 1, "{what_mode}, {what}: {apart} levels off the route off the chain");
            // Neither frame as it was: it blended.
            let unblended = run(&mut off, &graph(None), clip, at, pts).0;
            assert!(levels(&chain, &unblended) > 2, "{what_mode}, {what}: the blend changed the picture");
        }
    }
}
