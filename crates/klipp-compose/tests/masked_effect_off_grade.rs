//! A masked or mixed effect whose run is off its clip's grade (node-graph
//! M2 9c-2, PR B2): after a plug-in, under the camera's shutter, and on an
//! adjustment layer whose grade publishes the mask. The matte is the
//! grade's, and the effect runs where the grade's run doesn't reach. A
//! window reads no picture, so it is drawn in the effect's own plan and
//! the effect blended there, on the GPU where there is one
//! (`effects_blended_apart`, #1265); a matte that reads the grade's
//! picture (a key's) is made in a side run and blended on the CPU
//! (`effects_blended_on_cpu`). Never left unmasked
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
use klipp_timeline::test_clip::TestClip;

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

/// The counters for the effect the case masks: blended in its own plan
/// (`effects_blended_apart`, #1265), blended on the CPU
/// (`effects_blended_on_cpu`), and left unmasked for want of its matte
/// (`effects_mask_unavailable`).
fn blend_counters(stats: &RenderStats) -> (u32, u32, u32) {
    (stats.effects_blended_apart, stats.effects_blended_on_cpu, stats.effects_mask_unavailable)
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
    Keyframe { time_secs, value, interpolation: Default::default(), tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() }
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
    c = TestClip::of(c).effects(fx).build();
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
/// pieces differ (the case shows something); the counters say the masked
/// effect's window was blended in its own plan, none on the CPU, never
/// left unmasked, and that a mix with no mask needs no blend of its own
/// (it runs in the plan).
///
/// `cpu_bar`: the CPU route's bar. The pieces are renders of the clip with
/// no mix or mask, which the CPU kernels draw, while the masked clip's
/// plan runs on the CPU executor; the two agree to float noise except
/// where a shutter averages them (one level).
fn case(name: &str, build: impl Fn(Blend) -> Sequence, plugin: bool, keyed: bool, cpu_bar: f32) {
    let routes: Vec<bool> = if gpu() { vec![false, true] } else { vec![false] };
    for on_gpu in routes {
        let route = if on_gpu { "gpu" } else { "cpu" };
        let (input, _) = render(&build(Blend::Off), on_gpu, plugin);
        let (whole, _) = render(&build(Blend::Whole), on_gpu, plugin);
        assert!(worst(&input, &whole) > 0.02, "{name} ({route}): the blur changes the picture");
        let window = matte(keyed);
        let (lo, hi) = window.iter().fold((1.0f32, 0.0f32), |(a, b), p| (a.min(p[0]), b.max(p[0])));
        assert!(lo < 0.05 && hi > 0.95, "{name}: the window covers 0..1 ({lo}..{hi})");
        let bar = if on_gpu { BAR } else { cpu_bar };
        for (blend, want) in [(Blend::Masked, oracle(&input, &whole, Some(&window))), (Blend::Mixed, oracle(&input, &whole, None))] {
            let (got, stats) = render(&build(blend), on_gpu, plugin);
            assert!(worst(&got, &want) <= bar, "{name} ({route}, {blend:?}): {} from the oracle", worst(&got, &want));
            let want = if blend == Blend::Masked { (1, 0, 0) } else { (0, 0, 0) };
            assert_eq!(blend_counters(&stats), want, "{name} ({route}, {blend:?}): a window blended in the plan once, none on the CPU, a mix in the plan, never unmasked");
            // On the GPU the plan drew it whole: no effect run came down
            // to the CPU kernels or the CPU executor (#1265).
            if on_gpu {
                assert_eq!((stats.effects_on_cpu, stats.effects_on_cpu_kernels), (0, 0), "{name} ({blend:?}): the plan ran on the GPU");
            }
        }
    }
}

/// (a) A clip with a plug-in: its effects run as a plan after the plug-in
/// (11c-2), off the grade's run, where the window is made.
#[test]
fn a_masked_effect_after_a_plugin_is_blended_in_its_plan() {
    let build = |blend| with_invert(sequence(vec![vec![media(vec![], blend, false)]]), true);
    case("after a plug-in", build, true, false, 1e-5);
}

/// (b) A moving clip under the shutter, its blur after its transform:
/// the blur runs once on the samples' average at the frame's time, with
/// the window keyed in time and taken at that time (11c-3's shape).
#[test]
fn a_masked_effect_after_the_transform_under_the_shutter_is_blended_on_the_average() {
    let transform = E::Transform {
        position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0,
        anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
    };
    let build = |blend| {
        // Keyed on the picture-truth clip's placement, which it draws (#1537).
        let c = TestClip::of(media(vec![transform.clone()], blend, true)).lane("pos_x", 0.0, -12.0).lane("pos_x", 2.0, 12.0).build();
        let mut seq = sequence(vec![vec![c]]);
        seq.camera.shutter_deg = 180.0;
        seq.camera.shutter_samples = 8;
        seq
    };
    case("under the shutter, after the transform", build, false, true, BAR);
}

/// (b') The same with the blur before the transform: masked once at the
/// frame's time on the clip's own picture, then placed per sample, so the
/// window's edge smears with the motion. No closed-form oracle (the matte
/// moves with each sample); held to the CPU route, and to the input where
/// neither the window nor its sweep reaches.
#[test]
fn a_masked_effect_before_the_transform_under_the_shutter_smears_with_the_picture() {
    let build = |blend| {
        // Keyed on the picture-truth clip's placement, which it draws (#1537).
        let c = TestClip::of(media(vec![], blend, true)).lane("pos_x", 0.0, -12.0).lane("pos_x", 2.0, 12.0).build();
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
    assert_eq!(blend_counters(&cpu_stats), (1, 0, 0));
    if gpu() {
        let (on_gpu, stats) = render(&build(Blend::Masked), true, false);
        assert!(worst(&on_gpu, &cpu) <= BAR, "the GPU route is the CPU's within a level ({})", worst(&on_gpu, &cpu));
        assert_eq!(blend_counters(&stats), (1, 0, 0));
    }
}

/// (c) An adjustment layer whose grade publishes the window, its blur
/// masked by it, over a plain clip: the adjustment's effects run over the
/// composite below, off any clip's grade run.
#[test]
fn a_masked_effect_on_an_adjustment_layer_is_blended_in_its_plan() {
    let build = |blend| {
        let below = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
        // The first track is the top one: the adjustment over the clip.
        sequence(vec![vec![graded(ClipKind::AdjustmentLayer, vec![], blend, false)], vec![below]])
    };
    case("an adjustment layer", build, false, false, 1e-5);
}

/// A grade with a luma key beside its chain, reading the grade's Source
/// (the picture the grade reads), or, when `of_graded`, the Exposure's
/// output (a picture the grade made); and the key's id.
fn grade_with_key(of_graded: bool) -> (NodeGraph, NodeId) {
    let (mut g, window) = grade_with_window(false);
    let k = g.nodes.iter_mut().find(|n| n.id == window).unwrap();
    k.kind = NodeKind::LumaKey;
    k.params = NodeParams { luma_key: Some(klipp_core::LumaKeySettings { enabled: true, low: 0.3, high: 0.6, soft: 0.1, ..Default::default() }), ..Default::default() };
    if of_graded {
        let exposure = g.nodes.iter().find(|n| n.kind == NodeKind::Exposure).unwrap().id;
        for e in g.edges.iter_mut().filter(|e| e.to_node == window) {
            e.from_node = exposure;
        }
    }
    (g, window)
}

/// A media clip graded with a key beside its chain, its blur set as
/// `blend` by the key, after the test plug-in.
fn keyed_after_a_plugin(blend: Blend, of_graded: bool) -> Sequence {
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let (grade, key) = grade_with_key(of_graded);
    c.set_grade(grade);
    c = TestClip::of(c).effects(vec![E::GaussianBlur { radius: 5.0 }]).build();
    set_blend(&mut c, blend, key);
    with_invert(sequence(vec![vec![c]]), true)
}

/// The luma key's matte of the picture the grade reads (the chart), as
/// the key draws it: grey, opaque.
fn key_matte() -> Vec<[f32; 4]> {
    let SourcePixels::Linear(chart) = Chart.frame(&Clip::gap(tc(0.0), tc(1.0)), 0.0, W, H).unwrap().pixels else { unreachable!() };
    let k = klipp_core::LumaKeySettings { enabled: true, low: 0.3, high: 0.6, soft: 0.1, ..Default::default() };
    let luma = klipp_color::linear::luma_coefs(klipp_core::WorkingColorspace::default());
    let encode = |c: f32| klipp_color::transfer::linear_to_srgb(c);
    chart.chunks_exact(4).map(|p| {
        let m = klipp_color::luma_key::luma_key_value([encode(p[0]), encode(p[1]), encode(p[2])], &k, luma);
        [m, m, m, 1.0]
    }).collect()
}

/// #1265: a key of the picture the grade reads, apart from that grade, is
/// drawn in the effect's own plan from that picture handed to the run, on
/// the GPU where there is one: blended there, not on the CPU, and the
/// oracle's, the key's matte of the grade's input over the input and the
/// effect whole.
#[test]
fn a_key_of_the_grades_input_apart_from_it_is_blended_in_its_plan() {
    for on_gpu in if gpu() { vec![false, true] } else { vec![false] } {
        let (input, _) = render(&keyed_after_a_plugin(Blend::Off, false), on_gpu, true);
        let (whole, _) = render(&keyed_after_a_plugin(Blend::Whole, false), on_gpu, true);
        let (keyed, stats) = render(&keyed_after_a_plugin(Blend::Masked, false), on_gpu, true);
        assert_eq!(blend_counters(&stats), (1, 0, 0), "gpu {on_gpu}: the key blended in its plan");
        if on_gpu {
            assert_eq!((stats.effects_on_cpu, stats.effects_on_cpu_kernels), (0, 0), "the plan ran on the GPU");
        }
        let want = oracle(&input, &whole, Some(&key_matte()));
        assert!(worst(&keyed, &want) <= 2.0 * BAR, "gpu {on_gpu}: {} from the oracle", worst(&keyed, &want));
        // Control: the key keys some of the frame, so the blend is seen.
        assert!(worst(&keyed, &input) > 0.01 && worst(&keyed, &whole) > 0.01, "gpu {on_gpu}: neither the input nor the effect whole");
    }
}

/// A key of a picture the grade made can't be drawn apart from that grade:
/// it is still made in a side run and blended on the CPU, and counted so.
/// And #1268's review: a mix that can't be blended (not finite, as a file
/// can hold) is bypassed and said, the input left as it was, never counted
/// as blended.
#[test]
fn a_key_of_the_graded_picture_is_blended_on_the_cpu_and_a_bad_mix_bypassed() {
    for on_gpu in if gpu() { vec![false, true] } else { vec![false] } {
        let (input, _) = render(&keyed_after_a_plugin(Blend::Off, true), on_gpu, true);
        let (whole, _) = render(&keyed_after_a_plugin(Blend::Whole, true), on_gpu, true);
        let (keyed, stats) = render(&keyed_after_a_plugin(Blend::Masked, true), on_gpu, true);
        assert_eq!(blend_counters(&stats), (0, 1, 0), "gpu {on_gpu}: a key blended on the CPU");
        // Control: the key keys some of the frame, so the blend is seen.
        assert!(worst(&keyed, &input) > 0.01 && worst(&keyed, &whole) > 0.01, "gpu {on_gpu}: neither the input nor the effect whole");

        let mut seq = keyed_after_a_plugin(Blend::Masked, true);
        let c = &mut seq.tracks[0].clips[0];
        let mut stored = (*c.picture_v2().unwrap()).clone();
        // The blur, its only effect, by the picture's own node for it.
        let blur_id = c.effect_node_at(false, 0).expect("the blur");
        let blur = stored.picture.graph.nodes.iter_mut().find(|n| n.id == blur_id).unwrap();
        blur.mix = Some(f32::NAN);
        c.adopt_stored_picture(stored).expect("the picture is taken as stored");
        let (bypassed, stats) = render(&seq, on_gpu, true);
        assert_eq!(blend_counters(&stats), (0, 0, 1), "gpu {on_gpu}: a mix that can't be blended is unavailable, not blended");
        assert!(worst(&bypassed, &input) <= 1e-5, "gpu {on_gpu}: the input as it was ({})", worst(&bypassed, &input));
    }
}
