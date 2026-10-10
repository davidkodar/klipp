//! A picture-truth clip's masked and mixed effects, rendered (node-graph M2
//! step 9c-2): a blur mixed at a half and masked by a window, the window
//! either inside the clip's grade (published by the grade group) or beside
//! the clip's chain reading its source. Each draws what the parts give: the
//! graded picture, the blur of it, and the window's matte, as
//! `graded + (blurred - graded) × mix × matte`, on the GPU and on the CPU,
//! within the CPU/GPU bar of a level. A GPU run that can't record an
//! effect runs the same plan on the CPU, counted; a matte the renderer
//! can't give (wiring a file holds) leaves the effect bypassed, counted.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{Mask, MaskShape, MaskStack, RationalTime};
use klipp_effects::BuiltinEffect as E;
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::PortBinding;
use klipp_graph_lift::ops::GraphOp;
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};
use klipp_timeline::test_clip::TestClip;

const W: u32 = 96;
const H: u32 = 54;
const MIX: f32 = 0.5;

struct Chart;

impl FrameSource for Chart {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
                // Stripes, so a blur shows.
                let stripe = if (x / 4) % 2 == 0 { 0.8 } else { 0.1 };
                v.extend_from_slice(&[stripe * fx, 0.2 + 0.5 * fy, stripe * (1.0 - fx), 1.0]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
}

/// A circle in the frame's middle, feathered.
fn window_params() -> NodeParams {
    let masks = MaskStack { masks: vec![Mask { shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.3 }, feather: 0.1, ..Mask::default() }] };
    NodeParams { masks: Some(masks), ..Default::default() }
}

/// Where the window is.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Window {
    /// Inside the grade, reading its source; published by the grade group.
    InTheGrade,
    /// Beside the clip's chain, reading the clip's source.
    Beside,
}

/// The clip: an exposure grade, a blur (and, inside the grade, the window
/// when `window` is `InTheGrade`), and the window's id there.
fn clip(effects: Vec<E>, window: Window) -> (Clip, Option<NodeId>) {
    let source = node(NodeKind::Source, NodeParams::default());
    let exposure = node(NodeKind::Exposure, NodeParams { exposure: Some(0.3), ..Default::default() });
    let output = node(NodeKind::Output, NodeParams::default());
    let mut edges = vec![
        Edge { from_node: source.id, from_port: 0, to_node: exposure.id, to_port: 0 },
        Edge { from_node: exposure.id, from_port: 0, to_node: output.id, to_port: 0 },
    ];
    let mut nodes = vec![source.clone(), exposure, output.clone()];
    let mut window_id = None;
    if window == Window::InTheGrade {
        let w = node(NodeKind::Mask, window_params());
        edges.push(Edge { from_node: source.id, from_port: 0, to_node: w.id, to_port: 0 });
        window_id = Some(w.id);
        nodes.push(w);
    }
    let graph = NodeGraph { output_node_id: output.id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] };
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    c.set_grade(graph);
    let c = TestClip::of(c).effects(effects).build();
    (c, window_id)
}

fn blur() -> E {
    E::GaussianBlur { radius: 4.0 }
}

/// The ops that mask the clip's blur by its window and mix it at `MIX`.
fn masking(c: &Clip, window: Window, window_in_grade: Option<NodeId>) -> Vec<GraphOp> {
    use klipp_graph_format::ports::{input_key, output_key};
    let p = c.picture_v2().unwrap().picture.clone();
    let at = |ids: &[klipp_graph_format::NodeId]| NodePath::new(Root::Clip { clip: c.id.0 }, ids.iter().copied());
    let group = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).unwrap().id;
    let source = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
    let fx = p.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::types::LEGACY_EFFECT).unwrap().id;
    let mut ops = match window {
        Window::InTheGrade => vec![
            GraphOp::PublishOutput { group: at(&[group]), endpoint: PortBinding { node: window_in_grade.unwrap(), port: "out".into() }, key: "selection".into(), label: "Selection".into() },
            GraphOp::Connect { from: at(&[group]), from_port: "selection".into(), to: at(&[fx]), to_port: "mask".into() },
        ],
        Window::Beside => {
            let side = side_window();
            vec![
                GraphOp::AddNodeBeside { at: at(&[fx]), node: Box::new(side.clone()) },
                GraphOp::Connect { from: at(&[source]), from_port: "out".into(), to: at(&[side.id]), to_port: input_key("Mask", 0) },
                GraphOp::Connect { from: at(&[side.id]), from_port: output_key("Mask", 0), to: at(&[fx]), to_port: "mask".into() },
            ]
        }
    };
    ops.push(GraphOp::SetMix { path: at(&[fx]), mix: Some(MIX) });
    ops
}

/// A window as a node beside the chain: the lift's node for a grade's
/// Mask, with the window's settings.
fn side_window() -> klipp_graph_format::Node {
    let params = serde_json::to_value(window_params()).unwrap();
    klipp_graph_format::Node {
        id: klipp_graph_format::NodeId::from_u128(0x5e1de),
        type_id: klipp_graph_format::types::LEGACY_NODE.into(),
        type_version: 1,
        label: String::new(),
        enabled: true,
        params: Default::default(),
        anim: Default::default(),
        mix: None,
        wireless: Default::default(),
        group: None,
        legacy: Some(klipp_graph_format::Legacy { slot: None, payload: serde_json::json!({"kind": "Mask", "label": "Window", "params": params}), keyframes: Some(serde_json::json!({"tracks": []})), modifiers: Default::default() }),
        legacy_flags: None,
        origin: None,
    }
}

fn sequence(c: Clip) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![c];
    seq.tracks = vec![track];
    seq
}

fn renderer(gpu: bool) -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r
}

fn render(r: &mut Renderer, seq: &Sequence, inspect: Inspect) -> (Vec<f32>, klipp_compose::RenderStats) {
    let (out, found) = r.render_inspected(seq, tc(1.0), W, H, &mut Chart, RenderOptions::default(), inspect);
    (out, found.stats)
}

/// The oracle's parts for `window`, from renders of the clip as fields
/// (no mask, no mix): the graded picture, the blur of it, and the window's
/// matte (the grade's own window shown as its matte, or the same window in
/// a grade made to show it).
fn oracle(window: Window) -> Vec<f32> {
    let (graded_clip, w) = clip(vec![], Window::InTheGrade);
    let (blurred_clip, _) = clip(vec![blur()], Window::InTheGrade);
    let mut r = renderer(false);
    let graded = render(&mut r, &sequence(graded_clip.clone()), Inspect::default()).0;
    let blurred = render(&mut r, &sequence(blurred_clip), Inspect::default()).0;
    let id = graded_clip.id;
    let matte = render(&mut r, &sequence(graded_clip), Inspect { clip: Some(id), matte_view: w.map(|w| klipp_compose::inspect::grade_node(id, w)), ..Default::default() }).0;
    let _ = window;
    graded
        .chunks_exact(4)
        .zip(blurred.chunks_exact(4))
        .zip(matte.chunks_exact(4))
        .flat_map(|((g, b), m)| {
            let k = MIX * m[0].clamp(0.0, 1.0);
            [g[0] + (b[0] - g[0]) * k, g[1] + (b[1] - g[1]) * k, g[2] + (b[2] - g[2]) * k, g[3] + (b[3] - g[3]) * k]
        })
        .collect()
}

/// The worst difference in 8-bit sRGB levels, colour channels.
fn worst_levels(a: &[f32], b: &[f32]) -> u8 {
    let (a, b) = (klipp_effects::linear_f32_rgba_to_srgb_u8(a), klipp_effects::linear_f32_rgba_to_srgb_u8(b));
    a.iter().zip(&b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| x.abs_diff(*y)).max().unwrap_or(0)
}

/// The clip masked by `window`, as a sequence.
fn masked(window: Window) -> Sequence {
    let (mut c, w) = clip(vec![blur()], window);
    let ops = masking(&c, window, w);
    c.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{window:?}: the masking batch: {e}"));
    assert!(c.is_picture_truth());
    sequence(c)
}

fn gpu() -> bool {
    let ok = klipp_nodes::GraphExecutor::new().gpu_pipeline_or_init().is_some();
    assert!(ok || !required(), "required, but there is no GPU");
    ok
}

#[test]
fn a_masked_mixed_effect_draws_the_oracle_on_the_gpu_and_the_cpu() {
    for window in [Window::InTheGrade, Window::Beside] {
        let want = oracle(window);
        let seq = masked(window);
        // The CPU: the plan on the CPU executor, the CPU kernels never.
        let (cpu, stats) = render(&mut renderer(false), &seq, Inspect::default());
        assert_eq!(stats.effects_mask_unavailable, 0, "{window:?}");
        let worst = worst_levels(&cpu, &want);
        eprintln!("{window:?}: CPU against the oracle: worst {worst} levels");
        assert!(worst <= 1, "{window:?}: the CPU draws {worst} levels from the oracle");
        // The mask does something: not the plain blur, not the graded picture.
        let (plain_blur, _) = clip(vec![blur()], Window::InTheGrade);
        let blurred = render(&mut renderer(false), &sequence(plain_blur), Inspect::default()).0;
        assert!(worst_levels(&cpu, &blurred) > 2, "{window:?}: the mask and mix change the blur");
        // And the blur is there inside the window: not the graded picture.
        let (graded_clip, _) = clip(vec![], Window::InTheGrade);
        let graded = render(&mut renderer(false), &sequence(graded_clip), Inspect::default()).0;
        assert!(worst_levels(&cpu, &graded) > 2, "{window:?}: the blur shows where the window is");
        if !gpu() {
            eprintln!("UNVERIFIED: no GPU");
            continue;
        }
        let (on_gpu, stats) = render(&mut renderer(true), &seq, Inspect::default());
        assert_eq!(stats.effects_followed_grade, 1, "{window:?}: the effects ran in the grade's plan");
        assert_eq!((stats.effects_on_cpu, stats.effects_mask_unavailable), (0, 0), "{window:?}");
        let worst = worst_levels(&on_gpu, &want);
        eprintln!("{window:?}: GPU against the oracle: worst {worst} levels");
        assert!(worst <= 1, "{window:?}: the GPU draws {worst} levels from the oracle");
    }
}

/// A GPU run that can't record an effect (a lost device; forced) runs the
/// same plan on the CPU executor, counted: the mask and the mix are kept,
/// never dropped for the CPU kernels.
#[test]
fn a_masked_effect_the_gpu_cannot_record_runs_its_plan_on_the_cpu() {
    if !gpu() {
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let want = oracle(Window::InTheGrade);
    let seq = masked(Window::InTheGrade);
    let mut r = renderer(true);
    if let Some(g) = r.executor().gpu_pipeline_or_init() {
        g.refuse_recording(true);
    }
    let (out, stats) = render(&mut r, &seq, Inspect::default());
    assert_eq!(stats.effects_on_cpu, 1, "the plan ran again on the CPU, counted");
    assert_eq!(stats.effects_on_cpu_kernels, 1, "and counted as an effect run drawn on the CPU (#1274)");
    let worst = worst_levels(&out, &want);
    assert!(worst <= 1, "the CPU plan draws {worst} levels from the oracle");
}

/// The same forced retry, with a frei0r plug-in that isn't installed after
/// the masked effect: the frame names its decline once, the retry's, never
/// the first run's beside it (M4 step 10).
#[test]
fn a_plan_run_again_on_the_cpu_names_each_decline_once() {
    if !gpu() {
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let gone = E::Frei0r { type_id: "frei0r:/nowhere/gone.so:gone".into(), params: Vec::new() };
    let (mut c, w) = clip(vec![blur(), gone], Window::InTheGrade);
    let ops = masking(&c, Window::InTheGrade, w);
    c.apply_picture_ops(&ops).expect("the masking batch");
    let seq = sequence(c);
    let mut r = renderer(true);
    if let Some(g) = r.executor().gpu_pipeline_or_init() {
        g.refuse_recording(true);
    }
    let (_, found) = r.render_inspected(&seq, tc(1.0), W, H, &mut Chart, RenderOptions::default(), Inspect::default());
    assert_eq!(found.stats.effects_on_cpu, 1, "the plan ran again on the CPU");
    let missing: Vec<_> = found.declines.iter().filter(|d| d.outcome == klipp_nodes::DeclineOutcome::PluginMissing).collect();
    assert_eq!(missing.len(), 1, "one decline for the missing plug-in: {:?}", found.declines);
}

/// A file whose picture wires the mask from something the renderer can't
/// give (here, the clip's source, a node in its chain; the ops refuse
/// such wiring): the effect is bypassed, the picture as graded, said and
/// counted.
#[test]
fn a_mask_the_renderer_cannot_give_leaves_its_effect_bypassed() {
    let seq = masked(Window::InTheGrade);
    let c = &seq.tracks[0].clips[0];
    let mut stored = (*c.picture_v2().unwrap()).clone();
    let source = stored.picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
    for e in &mut stored.picture.graph.edges {
        if e.to.1.as_str() == "mask" {
            e.from = (source, "out".into());
        }
    }
    let (mut read, _) = clip(vec![blur()], Window::InTheGrade);
    read.id = c.id;
    read.set_grade(c.grade().clone());
    read.adopt_stored_picture(stored).unwrap();
    let (graded_clip, _) = clip(vec![], Window::InTheGrade);
    let graded = render(&mut renderer(false), &sequence(graded_clip), Inspect::default()).0;
    for gpu_on in [false, true] {
        if gpu_on && !gpu() {
            continue;
        }
        let (out, stats) = render(&mut renderer(gpu_on), &sequence(read.clone()), Inspect::default());
        assert_eq!(stats.effects_mask_unavailable, 1, "gpu {gpu_on}: counted");
        let worst = worst_levels(&out, &graded);
        assert!(worst <= 1, "gpu {gpu_on}: the bypassed effect leaves the graded picture ({worst} levels)");
    }
}

/// A file whose masked effect names a channel this Klipp doesn't read, or
/// reads an output its producer hasn't: neither is read as another (red,
/// the first output); the effect is bypassed, said and counted.
#[test]
fn an_unknown_channel_or_output_leaves_the_effect_bypassed() {
    let seq = masked(Window::InTheGrade);
    let c = &seq.tracks[0].clips[0];
    let (graded_clip, _) = clip(vec![], Window::InTheGrade);
    let graded = render(&mut renderer(false), &sequence(graded_clip), Inspect::default()).0;
    let edits: [(&str, Box<dyn Fn(&mut klipp_graph_format::lifted::ClipPicture)>); 2] = [
        ("an unknown channel", Box::new(|p| {
            let fx = p.graph.nodes.iter_mut().find(|n| n.type_id.as_str() == klipp_graph_format::types::LEGACY_EFFECT).unwrap();
            fx.params.insert("mask.channel".into(), klipp_graph_format::ParamValue::Text("purple".into()));
        })),
        ("an output the window hasn't", Box::new(|p| {
            let g = p.graph.nodes.iter_mut().find(|n| n.type_id.as_str() == lt::GROUP).unwrap();
            g.group.as_mut().unwrap().interface[0].binding.port = "nowhere".into();
        })),
    ];
    for (what, edit) in edits {
        let mut stored = (*c.picture_v2().unwrap()).clone();
        edit(&mut stored.picture);
        let (mut read, _) = clip(vec![blur()], Window::InTheGrade);
        read.id = c.id;
        read.set_grade(c.grade().clone());
        read.adopt_stored_picture(stored).unwrap();
        let (out, stats) = render(&mut renderer(false), &sequence(read), Inspect::default());
        assert_eq!(stats.effects_mask_unavailable, 1, "{what}: counted");
        assert!(worst_levels(&out, &graded) <= 1, "{what}: bypassed, the graded picture");
    }
}
