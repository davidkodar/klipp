//! A masked or mixed effect whose run is off its clip's grade (node-graph
//! M2 9c-2, PR B2): after a plug-in, under the camera's shutter, and on an
//! adjustment layer whose grade publishes the mask. The matte is made in
//! the grade, and the effect runs where the grade's run doesn't reach, so
//! the effect is drawn whole and blended with its input on the CPU
//! (`effects_blended_on_cpu`), never left unmasked
//! (`effects_mask_unavailable` stays 0).
//!
//! Each case renders through the renderer only, on the GPU and on the CPU,
//! and holds both to an oracle built from the same renderer's own pieces:
//! the frame without the effect (the input), the frame with the effect
//! whole (no mask, no mix), and the window rasterised at the frame's time,
//! mixed by `mask_mix` exactly as a mixed effect is. Under the shutter the
//! effect and its window are evaluated once at the frame's time (11c-3's
//! shape, ruled by klipp-b0 and klipp-4c): exact for an effect after the
//! clip's transform, which runs once on the average.
//!
//! Ignored until B2: the counters are named in [`blend_counters`], which B2
//! fills. A missing GPU leaves the GPU half unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

#![allow(dead_code)]

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, RenderStats, Renderer, SourceFrame, SourcePixels};
use klipp_core::{Keyframe, Mask, MaskShape, MaskStack, RationalTime};
use klipp_effects::mask_mix::{MaskChannel, MaskMix};
use klipp_effects::BuiltinEffect as E;
use klipp_graph_format::lifted::{types as lt, ClipPicture};
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::{NodeId, PortBinding};
use klipp_graph_lift::ops::GraphOp;
use klipp_nodes::{Edge, Node, NodeGraph, NodeKind, NodeParams};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

const W: u32 = 96;
const H: u32 = 54;
const MIX: f32 = 0.75;
/// The frame the cases are rendered at, and so the window's time.
const AT: f64 = 1.0;
/// A level of an 8-bit picture, in linear light.
const BAR: f32 = 1.0 / 255.0;

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// B2's two counters for the effect the case masks: blended on the CPU
/// (`effects_blended_on_cpu`) and left unmasked for want of its matte
/// (`effects_mask_unavailable`, from B1). Filled by B2, which adds the
/// first; until then the cases are ignored.
fn blend_counters(_stats: &RenderStats) -> (u32, u32) {
    todo!("9c-2 B2: (stats.effects_blended_on_cpu, stats.effects_mask_unavailable)")
}

/// Edges and a gradient in both halves, so a blur shows inside and outside
/// any window.
struct Chart;

impl FrameSource for Chart {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let v = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                let block = if ((x / 6) + (y / 6)) % 2 == 0 { 0.8 } else { 0.15 };
                [block * x as f32 / w as f32 + 0.05, block, block * (1.0 - y as f32 / h as f32) + 0.05, 1.0]
            })
            .collect();
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }
}

fn key(time_secs: f64, value: f32) -> Keyframe {
    Keyframe { time_secs, value, interpolation: Default::default(), tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0) }
}

/// A feathered circle, keyed across the clip when `keyed`.
fn window(keyed: bool) -> MaskStack {
    let mut m = Mask { shape: MaskShape::Circle { center_x: 0.45, center_y: 0.5, radius: 0.22 }, feather: 0.15, ..Mask::default() };
    if keyed {
        m.keyframes.center_x = vec![key(0.0, 0.3), key(2.0, 0.7)];
    }
    MaskStack { masks: vec![m] }
}

/// A grade with the window beside its chain (Source -> Exposure -> Output,
/// and a Mask off the source that nothing in the grade reads): the matte
/// the grade publishes for the effect.
fn grade_with_window(keyed: bool) -> (NodeGraph, NodeId) {
    let node = |kind: NodeKind, params: NodeParams| Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] };
    let s = node(NodeKind::Source, NodeParams::default());
    let e = node(NodeKind::Exposure, NodeParams { exposure: Some(0.1), ..Default::default() });
    let m = node(NodeKind::Mask, NodeParams { masks: Some(window(keyed)), ..Default::default() });
    let o = node(NodeKind::Output, NodeParams::default());
    let link = |a: &Node, b: &Node| Edge { from_node: a.id, from_port: 0, to_node: b.id, to_port: 0 };
    let edges = vec![link(&s, &e), link(&e, &o), link(&s, &m)];
    let mask = m.id;
    (NodeGraph { output_node_id: o.id, nodes: vec![s, e, m, o], edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }, mask)
}

/// How the case's blur is set.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Blend {
    /// Not at all: the effect whole.
    Whole,
    /// Mixed by `MIX` through the window on luminance.
    Masked,
    /// Mixed by `MIX`, no mask.
    Mixed,
    /// The effect taken off: the input.
    Off,
}

fn picture(c: &Clip) -> ClipPicture {
    c.picture_v2().expect("the clip lifts").picture.clone()
}

fn at(c: &Clip, ids: impl IntoIterator<Item = NodeId>) -> NodePath {
    NodePath::new(Root::Clip { clip: c.id.0 }, ids)
}

/// `clip`'s blur (its last effect) set as `blend`, through picture ops: a
/// mask and a mix make the clip picture-truth (#1259).
fn set_blend(clip: &mut Clip, blend: Blend, mask: NodeId) {
    let p = picture(clip);
    let blur = p.graph.nodes.iter().rfind(|n| n.type_id.as_str() == klipp_graph_format::types::LEGACY_EFFECT).expect("a blur").id;
    let group = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).expect("a grade group").id;
    let ops = match blend {
        Blend::Whole => vec![],
        Blend::Off => vec![GraphOp::RemoveNode { path: at(clip, [blur]) }],
        Blend::Mixed => vec![GraphOp::SetMix { path: at(clip, [blur]), mix: Some(MIX) }],
        Blend::Masked => vec![
            GraphOp::PublishOutput { group: at(clip, [group]), endpoint: PortBinding { node: mask, port: "out".into() }, key: "window".into(), label: "Window".into() },
            GraphOp::Connect { from: at(clip, [group]), from_port: "window".into(), to: at(clip, [blur]), to_port: "mask".into() },
            GraphOp::SetMaskChannel { path: at(clip, [blur]), channel: Some("luminance".into()) },
            GraphOp::SetMix { path: at(clip, [blur]), mix: Some(MIX) },
        ],
    };
    if !ops.is_empty() {
        clip.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{blend:?}: {e}"));
    }
}

/// A media clip graded with the window beside its chain, `before` its
/// blur, the blur set as `blend`.
fn media(before: Vec<E>, blend: Blend, keyed: bool) -> Clip {
    graded(ClipKind::Media { media_id: klipp_core::MediaId::new() }, before, blend, keyed)
}

/// [`media`] for a clip of `kind`, made that kind first: its picture is
/// lifted as that kind's.
fn graded(kind: ClipKind, before: Vec<E>, blend: Blend, keyed: bool) -> Clip {
    let mut c = Clip { kind, ..Clip::gap(tc(0.0), tc(2.0)) };
    let (grade, mask) = grade_with_window(keyed);
    c.set_grade(grade);
    let mut fx = before;
    fx.push(E::GaussianBlur { radius: 5.0 });
    let n = fx.len();
    c.set_effect_list(fx, vec![true; n]);
    set_blend(&mut c, blend, mask);
    c
}

/// `tracks`, the first the top one.
fn sequence(tracks: Vec<Vec<Clip>>) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = tracks
        .into_iter()
        .enumerate()
        .map(|(i, clips)| {
            let mut t = Track::new(&format!("V{}", i + 1), TrackKind::Video);
            t.clips = clips;
            t
        })
        .collect();
    seq
}

/// The test plug-in host's invert, on `seq`'s first clip, when `plugin`.
fn with_invert(mut seq: Sequence, plugin: bool) -> Sequence {
    if plugin {
        seq.tracks[0].clips[0].set_ofx_effects(vec![klipp_plugin::AppliedEffect { plugin_name: "com.klipp.test.Invert".into(), enabled: true, ..Default::default() }]);
    }
    seq
}

fn renderer(gpu: bool, plugin: bool) -> Renderer {
    let mut host = klipp_plugin::OfxHost::new();
    if plugin {
        let at = host.register_raw_plugin(klipp_plugin::test_plugin::create_test_plugin_raw());
        host.initialise_plugin(at).expect("the invert plug-in loads");
    }
    let mut r = Renderer::new(host, klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r
}

fn render(seq: &Sequence, gpu: bool, plugin: bool) -> (Vec<f32>, RenderStats) {
    let (out, found) = renderer(gpu, plugin).render_inspected(seq, tc(AT), W, H, &mut Chart, RenderOptions::default(), Inspect::default());
    (out, found.stats)
}

fn gpu() -> bool {
    let ok = klipp_nodes::GraphExecutor::new().gpu_pipeline_or_init().is_some();
    assert!(ok || !required(), "required, but there is no GPU");
    if !ok {
        eprintln!("UNVERIFIED: no GPU");
    }
    ok
}

fn rows(v: &[f32]) -> Vec<[f32; 4]> {
    v.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).collect()
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

/// The window at the frame's time, as the grade draws it: grey, opaque.
fn matte(keyed: bool) -> Vec<[f32; 4]> {
    let stack = window(keyed).evaluated_in(AT, H as f32 / W as f32);
    klipp_color::masks::generate_mask_matte(W, H, &stack).into_iter().map(|m| [m, m, m, 1.0]).collect()
}

/// `mask_mix` over the pieces: `input`, the effect `whole`, the window (or
/// none, white).
fn oracle(input: &[f32], whole: &[f32], matte: Option<&[[f32; 4]]>) -> Vec<f32> {
    let luma = klipp_color::linear::luma_coefs(klipp_core::WorkingColorspace::default());
    let mix = MaskMix::new(MIX, MaskChannel::Luminance, luma).unwrap();
    let mut out = vec![[0.0f32; 4]; (W * H) as usize];
    mix.apply(&rows(input), &rows(whole), matte, &mut out).unwrap();
    out.into_iter().flatten().collect()
}

/// One case: `build(blend)` gives the sequence. The masked and the mixed
/// render, on the CPU and the GPU, are each the oracle's over their own
/// route's pieces, within a level (the CPU within float noise); the
/// pieces differ (the case shows something); the counters say B2 blended
/// on the CPU and never left the effect unmasked.
fn case(name: &str, build: impl Fn(Blend) -> Sequence, plugin: bool, keyed: bool) {
    let routes: Vec<bool> = if gpu() { vec![false, true] } else { vec![false] };
    for on_gpu in routes {
        let route = if on_gpu { "gpu" } else { "cpu" };
        let (input, _) = render(&build(Blend::Off), on_gpu, plugin);
        let (whole, _) = render(&build(Blend::Whole), on_gpu, plugin);
        assert!(worst(&input, &whole) > 0.02, "{name} ({route}): the blur changes the picture");
        let window = matte(keyed);
        let (lo, hi) = window.iter().fold((1.0f32, 0.0f32), |(a, b), p| (a.min(p[0]), b.max(p[0])));
        assert!(lo < 0.05 && hi > 0.95, "{name}: the window covers 0..1 ({lo}..{hi})");
        let bar = if on_gpu { BAR } else { 1e-5 };
        for (blend, want) in [(Blend::Masked, oracle(&input, &whole, Some(&window))), (Blend::Mixed, oracle(&input, &whole, None))] {
            let (got, stats) = render(&build(blend), on_gpu, plugin);
            assert!(worst(&got, &want) <= bar, "{name} ({route}, {blend:?}): {} from the oracle", worst(&got, &want));
            let (blended, unavailable) = blend_counters(&stats);
            assert_eq!((blended, unavailable), (1, 0), "{name} ({route}, {blend:?}): blended on the CPU once, never unmasked");
        }
    }
}

/// (a) A clip with a plug-in: its effects run as a plan after the plug-in
/// (11c-2), off the grade's run, where the window is made.
#[test]
#[ignore = "9c-2 B2"]
fn a_masked_effect_after_a_plugin_is_blended_on_the_cpu() {
    let build = |blend| with_invert(sequence(vec![vec![media(vec![], blend, false)]]), true);
    case("after a plug-in", build, true, false);
}

/// (b) A moving clip under the shutter, its blur after its transform:
/// the blur runs once on the samples' average at the frame's time, with
/// the window keyed in time and taken at that time (11c-3's shape).
#[test]
#[ignore = "9c-2 B2"]
fn a_masked_effect_after_the_transform_under_the_shutter_is_blended_on_the_average() {
    let transform = E::Transform {
        position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0,
        anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
    };
    let build = |blend| {
        let mut c = media(vec![transform.clone()], blend, true);
        c.edit_transform_pos_x_keyframes(|v| klipp_core::keyframe::upsert_keyframe(v, 0.0, -12.0));
        c.edit_transform_pos_x_keyframes(|v| klipp_core::keyframe::upsert_keyframe(v, 2.0, 12.0));
        let mut seq = sequence(vec![vec![c]]);
        seq.camera.shutter_deg = 180.0;
        seq.camera.shutter_samples = 8;
        seq
    };
    case("under the shutter, after the transform", build, false, true);
}

/// (b') The same with the blur before the transform: masked once at the
/// frame's time on the clip's own picture, then placed per sample, so the
/// window's edge smears with the motion. No closed-form oracle (the matte
/// moves with each sample); held to the CPU route, and to the input where
/// neither the window nor its sweep reaches.
#[test]
#[ignore = "9c-2 B2"]
fn a_masked_effect_before_the_transform_under_the_shutter_smears_with_the_picture() {
    let build = |blend| {
        let mut c = media(vec![], blend, true);
        c.edit_transform_pos_x_keyframes(|v| klipp_core::keyframe::upsert_keyframe(v, 0.0, -12.0));
        c.edit_transform_pos_x_keyframes(|v| klipp_core::keyframe::upsert_keyframe(v, 2.0, 12.0));
        let mut seq = sequence(vec![vec![c]]);
        seq.camera.shutter_deg = 180.0;
        seq.camera.shutter_samples = 8;
        seq
    };
    let (cpu, cpu_stats) = render(&build(Blend::Masked), false, false);
    let (input, _) = render(&build(Blend::Off), false, false);
    // The left edge column is outside the window and its sweep.
    let far = |v: &[f32]| (0..H).flat_map(|y| { let i = (y * W * 4) as usize; v[i..i + 3].to_vec() }).collect::<Vec<f32>>();
    assert!(worst(&far(&cpu), &far(&input)) <= 1e-5, "outside the window's sweep, the input");
    assert!(worst(&cpu, &input) > 0.02, "inside, the blur");
    assert_eq!(blend_counters(&cpu_stats), (1, 0));
    if gpu() {
        let (on_gpu, stats) = render(&build(Blend::Masked), true, false);
        assert!(worst(&on_gpu, &cpu) <= BAR, "the GPU route is the CPU's within a level ({})", worst(&on_gpu, &cpu));
        assert_eq!(blend_counters(&stats), (1, 0));
    }
}

/// (c) An adjustment layer whose grade publishes the window, its blur
/// masked by it, over a plain clip: the adjustment's effects run over the
/// composite below, off any clip's grade run.
#[test]
#[ignore = "9c-2 B2"]
fn a_masked_effect_on_an_adjustment_layer_is_blended_on_the_cpu() {
    let build = |blend| {
        let below = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
        // The first track is the top one: the adjustment over the clip.
        sequence(vec![vec![graded(ClipKind::AdjustmentLayer, vec![], blend, false)], vec![below]])
    };
    case("an adjustment layer", build, false, false);
}
