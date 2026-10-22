//! A heavy node's output held for the parked frame (#1863): a slider
//! step below temporal noise reduction or an OFX plug-in takes the
//! node's output from the executor's cache instead of running it, and
//! draws exactly what a run without the cache draws, on the CPU and on
//! the executor's GPU. Without the parked frame's source nothing is held.
//! A missing GPU leaves the GPU half unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use klipp_core::{ClipId, MediaId, OfxValue};
use klipp_effects::BuiltinEffect as E;
use klipp_nodes::frame_context::{BoundedSources, EncodedFrame, FrameContext, LinearFrame, Neighbour, NoAnalysis, PluginInstances, SourceProvider};
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::held::{HeldSource, Retime};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::plugin::{OfxDescription, OfxInstance, OfxNodeHost, OfxNodeParams, Refusal};
use klipp_nodes::{Decline, GraphExecutor};

const W: u32 = 8;
const H: u32 = 4;
const FPS: f64 = 25.0;
/// The parked frame's clip and source time.
const AT: f64 = 0.4;

fn frame() -> Vec<f32> {
    (0..W * H).flat_map(|i| [i as f32 / 32.0, 1.0 - i as f32 / 32.0, 0.5, 1.0]).collect()
}

/// A media of `end` seconds whose every frame is its own grey, so a
/// neighbour of another time is another picture.
struct Media {
    end: f64,
}

impl SourceProvider for Media {
    fn bounds(&self) -> (f64, f64) {
        (0.0, self.end)
    }
    fn frame_secs(&self) -> Option<f64> {
        Some(1.0 / FPS)
    }
    fn encoded(&mut self, source_secs: f64) -> Neighbour<EncodedFrame> {
        Neighbour::Frame(EncodedFrame { rgba8: vec![(source_secs * 100.0) as u8; (W * H * 4) as usize], width: W, height: H, source_secs })
    }
    fn linear(&mut self, source_secs: f64) -> Neighbour<LinearFrame> {
        let grey = (source_secs * 0.7).fract() as f32;
        Neighbour::Frame(LinearFrame { rgba: Arc::new((0..W * H).flat_map(|_| [grey, grey * 0.5, 1.0 - grey, 1.0]).collect()), width: W, height: H, source_secs, pts: None })
    }
}

/// An OFX host of one filter, `swap`, counting its renders, and one that
/// renders in sequence, `sequential`.
#[derive(Default, Clone)]
struct Host {
    renders: Arc<Mutex<usize>>,
}

struct Swap {
    renders: Arc<Mutex<usize>>,
}

impl OfxNodeHost for Host {
    fn describe(&self, plugin: &str) -> Result<OfxDescription, Refusal> {
        Ok(OfxDescription { filter: true, budget: 0, sequential_render: plugin == "sequential", unsafe_threads: false, schema: 0 })
    }
    fn instance(&self, _: &str, _: f64) -> Result<Box<dyn OfxInstance>, Refusal> {
        Ok(Box::new(Swap { renders: Arc::clone(&self.renders) }))
    }
}

impl OfxInstance for Swap {
    fn frames_needed(&mut self, _: &HashMap<String, OfxValue>, _: f64) -> Vec<f64> {
        vec![]
    }
    fn render(&mut self, _: &HashMap<String, OfxValue>, rgba8: &[u8], _: u32, _: u32, _: f64, _: f64, _: &[(f64, EncodedFrame)]) -> Result<Vec<u8>, Refusal> {
        *self.renders.lock().unwrap() += 1;
        Ok(rgba8.chunks_exact(4).flat_map(|p| [p[2], 255 - p[0], p[1], p[3]]).collect())
    }
    fn bytes(&self) -> usize {
        0
    }
}

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
}

fn denoise() -> Node {
    node(NodeKind::Effect, NodeParams { effects: vec![E::TemporalDenoise { strength: 1.0, threshold: 0.5 }], ..Default::default() })
}

fn ofx_node(plugin: &str) -> Node {
    node(NodeKind::Ofx, NodeParams { ofx: Some(OfxNodeParams { plugin: plugin.into(), values: HashMap::new(), filter: true }), ..Default::default() })
}

fn exposure(stops: f32) -> Node {
    node(NodeKind::Exposure, NodeParams { exposure: Some(stops), ..Default::default() })
}

/// The nodes in a line, Source first and Output last.
fn chain(nodes: Vec<Node>) -> NodeGraph {
    let edges = nodes.windows(2).map(|w| Edge { from_node: w[0].id, from_port: 0, to_node: w[1].id, to_port: 0 }).collect();
    NodeGraph { output_node_id: nodes.last().unwrap().id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

/// Source → `heavy` → Exposure → Output; the Exposure node's id.
fn below(heavy: Node) -> (NodeGraph, NodeId) {
    let ex = exposure(0.5);
    let id = ex.id;
    (chain(vec![node(NodeKind::Source, NodeParams::default()), heavy, ex, node(NodeKind::Output, NodeParams::default())]), id)
}

fn set_stops(g: &mut NodeGraph, id: NodeId, stops: f32) {
    g.nodes.iter_mut().find(|n| n.id == id).unwrap().params.exposure = Some(stops);
}

fn source(clip: ClipId) -> HeldSource {
    HeldSource {
        clip,
        media: MediaId::nil(),
        path: "/take.mov".into(),
        file_len: 1000,
        file_modified: std::time::SystemTime::UNIX_EPOCH,
        log_format: None,
        pts_micros: (AT * 1e6) as i64,
        decoded: (W, H),
        retime: Retime { speed: 1.0, reverse: false, source_at_clip_time: AT, media_fps: Some(FPS), conform_fps: None },
        proxy: None,
    }
}

/// What a run gives: its picture, and what it declined.
struct Ran {
    pixels: Vec<f32>,
    declines: Vec<Decline>,
}

/// One render of `g` for `clip` at `AT`, with `held` the parked frame's
/// source given around it, as the viewer gives it.
fn run(ex: &mut GraphExecutor, g: &NodeGraph, clip: ClipId, held: Option<HeldSource>, media: &mut Media, host: &Host, tap: Option<NodeId>) -> Ran {
    ex.hold_outputs_for(held);
    let mut bounded = BoundedSources::new(media, W, H);
    let mut instances = PluginInstances::default();
    let mut ctx = FrameContext {
        clip,
        frame: 10,
        source: (MediaId::nil(), 0.0),
        clip_time_secs: AT,
        source_secs: AT,
        source_pts: Some(AT),
        frame_number: 10,
        fps: FPS,
        sources: &mut bounded,
        analysis: &NoAnalysis,
        instances: &mut instances,
        ofx: Some(host),
        source_at: &|t| t,
    };
    let pixels = ex.execute_linear_with_context(&frame(), g, W, H, AT, 10, tap, None, None, &mut ctx).expect("the run").0;
    ex.hold_outputs_for(None);
    Ran { pixels, declines: ex.last_run_declines().to_vec() }
}

fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|f| f.to_bits()).collect()
}

/// The GPU executor with its temporal denoise off the chain (#1988): a
/// parked frame's denoise is held as pixels, so is drawn off the chain
/// (on the GPU since #2266), and a run without the cache is held to it
/// bit for bit on the same route.
/// The chain's own denoise is held to the CPU's by `denoise_on_chain`.
fn gpu_cpu_denoise() -> GraphExecutor {
    let mut ex = GraphExecutor::new();
    ex.set_denoise_on_chain(false);
    ex
}

/// The executors to check: the CPU one, and the GPU one where there is a
/// GPU.
fn executors() -> Vec<(&'static str, fn() -> GraphExecutor)> {
    let mut all: Vec<(&'static str, fn() -> GraphExecutor)> = vec![("cpu", GraphExecutor::new_cpu_only)];
    let mut probe = GraphExecutor::new();
    let ok = probe.gpu_pipeline_or_init().is_some();
    let required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1");
    assert!(ok || !required, "required, but there is no GPU");
    if ok {
        all.push(("gpu", gpu_cpu_denoise));
    } else {
        eprintln!("UNVERIFIED: no GPU");
    }
    all
}

/// A 16-step drag of the Exposure below the heavy node: the heavy node
/// runs once, every later step takes it held, and every step's picture
/// is bit for bit an uncached run's.
fn drag_below(heavy: fn() -> Node, renders: fn(&GraphExecutor, &Host) -> u64) {
    for (arm, make) in executors() {
        let (mut g, stops) = below(heavy());
        let clip = ClipId::new();
        let (mut held, mut plain) = (make(), make());
        let (host, plain_host) = (Host::default(), Host::default());
        let mut media = Media { end: 10.0 };
        for step in 0..16 {
            set_stops(&mut g, stops, 0.5 + step as f32 * 0.05);
            let a = run(&mut held, &g, clip, Some(source(clip)), &mut media, &host, None);
            let b = run(&mut plain, &g, clip, None, &mut media, &plain_host, None);
            assert_eq!(bits(&a.pixels), bits(&b.pixels), "{arm}: step {step} differs from a run without the cache");
            assert_eq!(a.declines, b.declines, "{arm}: step {step} declines differently");
        }
        assert_eq!(renders(&held, &host), 1, "{arm}: the heavy node ran once in 16 steps");
        assert_eq!(renders(&plain, &plain_host), 16, "{arm}: without the cache it ran each step");
        let counts = held.held_counts();
        assert_eq!((counts.hits, counts.misses, counts.stored), (15, 1, 1), "{arm}: {counts:?}");
        assert_eq!(plain.held_counts(), Default::default(), "{arm}: nothing is held without the source");
    }
}

#[test]
fn a_drag_below_a_denoise_runs_it_once() {
    drag_below(denoise, |ex, _| ex.source_runs());
}

#[test]
fn a_drag_below_a_plugin_runs_it_once() {
    drag_below(|| ofx_node("swap"), |_, host| *host.renders.lock().unwrap() as u64);
}

/// Each step after a change above the node, or to its source or time,
/// is a miss, and the picture is an uncached run's: never the held one.
#[test]
fn a_change_above_the_node_is_a_miss() {
    type Change = Box<dyn Fn(&mut NodeGraph, &mut HeldSource)>;
    for (arm, make) in executors() {
        let up = exposure(0.25);
        let up_id = up.id;
        let heavy = denoise();
        let heavy_id = heavy.id;
        let base = chain(vec![node(NodeKind::Source, NodeParams::default()), up, heavy, exposure(0.5), node(NodeKind::Output, NodeParams::default())]);
        let clip = ClipId::new();
        let changes: Vec<(&str, Change)> = vec![
            ("an edit above", Box::new(move |g, _| g.nodes.iter_mut().find(|n| n.id == up_id).unwrap().params.exposure = Some(0.75))),
            ("the node above switched off", Box::new(move |g, _| g.nodes.iter_mut().find(|n| n.id == up_id).unwrap().enabled = false)),
            ("the node's strength", Box::new(move |g, _| g.nodes.iter_mut().find(|n| n.id == heavy_id).unwrap().params.effects = vec![E::TemporalDenoise { strength: 0.5, threshold: 0.5 }])),
            ("another frame", Box::new(|_, s| s.pts_micros += 40_000)),
            ("the file rewritten", Box::new(|_, s| s.file_len += 1)),
            ("another speed", Box::new(|_, s| s.retime.speed = 0.5)),
        ];
        for (what, change) in changes {
            let (mut held, mut plain) = (make(), make());
            let host = Host::default();
            let mut media = Media { end: 10.0 };
            let mut s = source(clip);
            run(&mut held, &base, clip, Some(s.clone()), &mut media, &host, None);
            let mut g = base.clone();
            change(&mut g, &mut s);
            let misses = held.held_counts().misses;
            let a = run(&mut held, &g, clip, Some(s), &mut media, &host, None);
            let b = run(&mut plain, &g, clip, None, &mut media, &host, None);
            assert_eq!(held.held_counts().misses, misses + 1, "{arm}: {what} is a miss");
            assert_eq!(bits(&a.pixels), bits(&b.pixels), "{arm}: {what} draws the uncached picture");
        }
    }
}

/// A Blend below the held node that also reads the Source: the Source
/// still runs, and the picture is the uncached one.
#[test]
fn a_source_read_from_below_still_runs() {
    for (arm, make) in executors() {
        let s = node(NodeKind::Source, NodeParams::default());
        let heavy = denoise();
        let ex = exposure(0.5);
        let blend = node(NodeKind::Blend, NodeParams { mix: Some(0.5), ..Default::default() });
        let out = node(NodeKind::Output, NodeParams::default());
        let link = |a: &Node, b: &Node, port| Edge { from_node: a.id, from_port: 0, to_node: b.id, to_port: port };
        let edges = vec![link(&s, &heavy, 0), link(&heavy, &ex, 0), link(&ex, &blend, 0), link(&s, &blend, 1), link(&blend, &out, 0)];
        let g = NodeGraph { output_node_id: out.id, nodes: vec![s, heavy, ex, blend, out], edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] };
        let clip = ClipId::new();
        let (mut held, mut plain) = (make(), make());
        let host = Host::default();
        let mut media = Media { end: 10.0 };
        run(&mut held, &g, clip, Some(source(clip)), &mut media, &host, None);
        let a = run(&mut held, &g, clip, Some(source(clip)), &mut media, &host, None);
        let b = run(&mut plain, &g, clip, None, &mut media, &host, None);
        assert_eq!(held.held_counts().hits, 1, "{arm}");
        assert_eq!(bits(&a.pixels), bits(&b.pixels), "{arm}: the blend's other side is the source");
    }
}

/// The frame after the media's last has no neighbour: the denoise runs
/// against one side and declines the other. A hit declines the same.
#[test]
fn a_hit_declines_what_the_run_declined() {
    let (g, _) = below(denoise());
    let clip = ClipId::new();
    let mut held = GraphExecutor::new_cpu_only();
    let host = Host::default();
    let mut media = Media { end: AT + 0.01 };
    let first = run(&mut held, &g, clip, Some(source(clip)), &mut media, &host, None);
    assert!(!first.declines.is_empty(), "the frame past the media is declined");
    let again = run(&mut held, &g, clip, Some(source(clip)), &mut media, &host, None);
    assert_eq!(held.held_counts().hits, 1);
    assert_eq!(again.declines, first.declines);
}

/// Another clip's run, a plug-in with history, and a tapped node above
/// the heavy one: nothing held, nothing taken.
#[test]
fn what_is_not_held() {
    let clip = ClipId::new();
    let host = Host::default();
    let mut media = Media { end: 10.0 };

    let (g, _) = below(denoise());
    let mut ex = GraphExecutor::new_cpu_only();
    run(&mut ex, &g, ClipId::new(), Some(source(clip)), &mut media, &host, None);
    assert_eq!(ex.held_counts(), Default::default(), "a run of another clip ignores the source");

    let (g, _) = below(ofx_node("sequential"));
    let mut ex = GraphExecutor::new_cpu_only();
    run(&mut ex, &g, clip, Some(source(clip)), &mut media, &host, None);
    run(&mut ex, &g, clip, Some(source(clip)), &mut media, &host, None);
    assert_eq!((ex.held_counts().hits, ex.held_counts().stored), (0, 0), "a plug-in rendering in sequence");

    let (g, _) = below(denoise());
    let tapped = g.nodes[0].id;
    let mut ex = GraphExecutor::new_cpu_only();
    let mut plain = GraphExecutor::new_cpu_only();
    run(&mut ex, &g, clip, Some(source(clip)), &mut media, &host, Some(tapped));
    let a = run(&mut ex, &g, clip, Some(source(clip)), &mut media, &host, Some(tapped));
    let b = run(&mut plain, &g, clip, None, &mut media, &host, Some(tapped));
    assert_eq!((ex.held_counts().hits, ex.held_counts().stored), (0, 0), "the Source tapped");
    assert_eq!(bits(&a.pixels), bits(&b.pixels));
}

/// A denoise and a plug-in below it, both held through a drag, even
/// under a budget smaller than the two: two entries always stay, so a
/// 4K pair (or one 8K entry) is never evicted by its partner.
#[test]
fn a_denoise_and_a_plugin_both_stay() {
    let clip = ClipId::new();
    let host = Host::default();
    let mut media = Media { end: 10.0 };
    let ex_node = exposure(0.5);
    let stops = ex_node.id;
    let mut g = chain(vec![node(NodeKind::Source, NodeParams::default()), denoise(), ofx_node("swap"), ex_node, node(NodeKind::Output, NodeParams::default())]);
    let mut ex = GraphExecutor::new_cpu_only();
    // A budget smaller than both: the minimum of two keeps them.
    ex.set_held_budget(1);
    for step in 0..4 {
        set_stops(&mut g, stops, 0.5 + step as f32 * 0.1);
        run(&mut ex, &g, clip, Some(source(clip)), &mut media, &host, None);
    }
    assert_eq!(ex.held_len(), 2);
    assert_eq!(*host.renders.lock().unwrap(), 1, "the plug-in ran once");
    assert_eq!(ex.source_runs(), 1, "the denoise ran once");
    ex.clear_held();
    assert_eq!(ex.held_len(), 0);
}

/// The viewer's GPU path runs the parked frame's grade with no frame
/// context (#1863, step B3): it takes the heavy node held (a denoise, an
/// OFX plug-in), starting the chain under it from the held output's GPU
/// copy where there is a GPU, and draws bit for bit what the renderer's
/// run draws. A step whose output isn't held fails, so the renderer draws
/// it, rather than run the node without its context.
#[test]
fn a_run_without_context_takes_only_what_is_held() {
    for (arm, make) in executors() {
        for heavy in [denoise as fn() -> Node, || ofx_node("swap")] {
            let (mut g, stops) = below(heavy());
            let clip = ClipId::new();
            let (mut held, mut plain) = (make(), make());
            let host = Host::default();
            let mut media = Media { end: 10.0 };
            run(&mut held, &g, clip, Some(source(clip)), &mut media, &host, None);
            for step in 0..3 {
                set_stops(&mut g, stops, 0.6 + step as f32 * 0.1);
                held.hold_outputs_for(Some(source(clip)));
                let a = held.execute_linear_with_thumbs(&frame(), &g, W, H, AT, 10, None, None, None).expect("a held run").0;
                held.hold_outputs_for(None);
                let b = run(&mut plain, &g, clip, None, &mut media, &host, None);
                assert_eq!(bits(&a), bits(&b.pixels), "{arm}: step {step} differs from the renderer's run");
            }
            assert_eq!(held.held_counts().hits, 3, "{arm}");
            assert_eq!(held.held_counts().stored, 1, "{arm}: the node ran for the renderer only");
            if arm == "gpu" {
                assert_eq!(held.held_gpu_len(), 1, "the chain started from the held output's GPU copy");
            }
            let mut other = source(clip);
            other.pts_micros += 40_000;
            held.hold_outputs_for(Some(other));
            let missed = held.execute_linear_with_thumbs(&frame(), &g, W, H, AT, 10, None, None, None);
            held.hold_outputs_for(None);
            assert!(matches!(missed, Err(klipp_nodes::executor::ExecutorError::CpuSourceRequired)), "{arm}: a miss without context must fail, not run the node blind");
            assert_eq!(held.held_counts().stored, 1, "{arm}");
        }
    }
}

/// A GPU path's run seeded with the parked frame's picture, whose Source
/// a held output stands in for (#1863): the seed is the Source's, never
/// what a chain under the held output starts from. Here the held denoise
/// is read twice (an Exposure and a Blend), so no chain starts from its
/// GPU copy, and the Exposure's chain must start from its pixels.
#[test]
fn a_seed_whose_source_is_held_is_not_graded() {
    let mut ex = GraphExecutor::new();
    if ex.gpu_pipeline_or_init().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let s = node(NodeKind::Source, NodeParams::default());
    let heavy = denoise();
    let up = exposure(0.5);
    let blend = node(NodeKind::Blend, NodeParams { mix: Some(0.5), ..Default::default() });
    let out = node(NodeKind::Output, NodeParams::default());
    let link = |a: &Node, b: &Node, port| Edge { from_node: a.id, from_port: 0, to_node: b.id, to_port: port };
    let edges = vec![link(&s, &heavy, 0), link(&heavy, &up, 0), link(&up, &blend, 0), link(&heavy, &blend, 1), link(&blend, &out, 0)];
    let g = NodeGraph { output_node_id: out.id, nodes: vec![s, heavy, up, blend, out], edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] };
    let clip = ClipId::new();
    let host = Host::default();
    let mut media = Media { end: 10.0 };
    let want = run(&mut ex, &g, clip, Some(source(clip)), &mut media, &host, None).pixels;
    // Another picture in the seed: what the GPU path's pre-grade would be.
    let other: Vec<f32> = frame().iter().map(|v| 1.0 - v).collect();
    assert!(ex.prime_chain_from_linear(&other, W, H));
    ex.hold_outputs_for(Some(source(clip)));
    let got = ex.execute_linear_with_thumbs(&[], &g, W, H, AT, 10, None, None, None).expect("a held run").0;
    ex.hold_outputs_for(None);
    assert_eq!(ex.held_counts().hits, 1);
    assert_eq!(bits(&got), bits(&want), "the chain under the held output read the seed");
}

/// A parked frame's denoise is held as pixels, so it is drawn off the
/// chain (#1988); since #2266 on the GPU with the chain's kernel, so the
/// picture is the one the same frame unparked draws on the chain, and
/// the buffer it was drawn in is the held entry's GPU copy, no upload.
/// The next step takes it held, the picture unchanged.
#[test]
fn a_parked_denoise_draws_the_picture_an_unparked_one_draws() {
    let mut probe = GraphExecutor::new();
    if probe.gpu_pipeline_or_init().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let (g, _) = below(denoise());
    let clip = ClipId::new();
    let host = Host::default();
    let mut media = Media { end: 10.0 };
    let (mut parked, mut playing) = (GraphExecutor::new(), GraphExecutor::new());
    let a = run(&mut parked, &g, clip, Some(source(clip)), &mut media, &host, None);
    assert_eq!(parked.last_denoise_on_chain(), 0, "parked: off the chain");
    assert_eq!(parked.held_counts().stored, 1, "and held");
    assert_eq!(parked.held_gpu_len(), 1, "with its GPU copy, from the draw");
    let b = run(&mut playing, &g, clip, None, &mut media, &host, None);
    assert_eq!(playing.last_denoise_on_chain(), 1, "unparked: the chain");
    let srgb8 = |v: f32| (klipp_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as i32;
    let off = a.pixels.iter().zip(&b.pixels).map(|(x, y)| (srgb8(*x) - srgb8(*y)).abs()).max().unwrap();
    assert!(off <= 1, "{off} levels between the parked and the unparked picture");
    let again = run(&mut parked, &g, clip, Some(source(clip)), &mut media, &host, None);
    assert_eq!(parked.held_counts().hits, 1, "the next step takes it held");
    assert_eq!(bits(&again.pixels), bits(&a.pixels), "the same picture");
}
