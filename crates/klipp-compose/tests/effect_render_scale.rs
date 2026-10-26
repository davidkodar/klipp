//! Every effect looks the same at any render size (node-graph M2 step 13,
//! the acceptance spec's G2). An effect measured in pixels (a blur's
//! radius, a streak's length, a block, a whole radius) is authored at the
//! sequence's size; a half-size render applies it scaled
//! (`at_render_scale`, whose rules the registry describes), so the half
//! render looks like the full one shrunk. The old behaviour, the effect
//! in the half render's own pixels, is the same effect on a sequence
//! that is itself half the size: the scaled render has to be clearly
//! closer to the shrunk full one than that, and close in its own right.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, Resolution};
use klipp_effects::registry::{effect_types, ParamKind};
use klipp_effects::BuiltinEffect;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};
use klipp_timeline::test_clip::TestClip;

const W: u32 = 192;
const H: u32 = 108;

/// Hue across, brightness down, with hard-edged blocks so a blur, a
/// shrink or a clarity pass has edges to act on. Drawn in the frame's
/// own coordinates, so the half-size frame is the full one shrunk.
struct Chart;

impl FrameSource for Chart {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (u, t) = (x as f32 / w as f32, y as f32 / h as f32);
                let block = if ((u * 8.0) as u32 + (t * 4.0) as u32).is_multiple_of(2) { 1.0 } else { 0.55 };
                let hue = u * 6.0;
                let f = |n: f32| {
                    let k = (n + hue) % 6.0;
                    1.0 - (k.min(4.0 - k).clamp(0.0, 1.0)) * 0.7
                };
                let level = (0.15 + 0.7 * (1.0 - t)) * block;
                v.extend_from_slice(&[f(5.0) * level, f(3.0) * level, f(1.0) * level, 1.0]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

/// The clip carrying `effect`, in a sequence `seq` pixels in size,
/// rendered `w` by `h`.
fn render(effect: &BuiltinEffect, seq: (u32, u32), w: u32, h: u32, gpu: bool) -> Vec<f32> {
    let clip = TestClip::of(Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) }).effect(effect.clone()).build();
    let mut s = Sequence::new_default();
    s.resolution = Resolution { width: seq.0, height: seq.1 };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip];
    s.tracks = vec![track];
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.render(&s, tc(1.0), w, h, &mut Chart, RenderOptions::default())
}

/// `full` shrunk 2x by averaging each 2x2 block.
fn shrink(full: &[f32]) -> Vec<f32> {
    let (w, h) = (W / 2, H / 2);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            for c in 0..4 {
                let at = |dx: u32, dy: u32| full[(((2 * y + dy) * W + 2 * x + dx) * 4 + c) as usize];
                out.push((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) * 0.25);
            }
        }
    }
    out
}

fn mean_diff(a: &[f32], b: &[f32]) -> f32 {
    let d: f32 = a.chunks_exact(4).zip(b.chunks_exact(4)).map(|(p, q)| (0..3).map(|c| (p[c] - q[c]).abs()).sum::<f32>()).sum();
    d / (a.len() / 4 * 3) as f32
}

/// What makes an effect act on the chart where its sample is too gentle
/// to see: a crop's edges for its feather to soften, thresholds low
/// enough for the chart's brightness to bloom and streak, and the median
/// filter's radius at the 2 it is clamped to (a 4 and its half would both
/// run as 2).
fn visible(tag: &str, v: &mut serde_json::Value) {
    match tag {
        "crop" => {
            for side in ["left", "right", "top", "bottom"] {
                v[side] = serde_json::json!(0.15);
            }
        }
        "bloom" | "anamorphic_streak" => v["threshold"] = serde_json::json!(0.2),
        "film_halation" => {
            v["threshold"] = serde_json::json!(0.2);
            v["intensity"] = serde_json::json!(1.0);
        }
        "diffusion" => {
            v["threshold"] = serde_json::json!(0.1);
            v["amount"] = serde_json::json!(0.8);
        }
        "median_filter" => v["radius"] = serde_json::json!(2),
        // A ripple, a gap and bands this small frame still shows below
        // its line (#2707): most of their ranges reach past it.
        "reflection" => {
            v["waterline"] = serde_json::json!(0.4);
            v["gap"] = serde_json::json!(6.0);
            v["ripple_amount"] = serde_json::json!(5.0);
            v["ripple_size"] = serde_json::json!(20.0);
        }
        // A move the frame still shows: most of its range sends the
        // picture out of it.
        "picture_transform" => {
            v["position_x"] = serde_json::json!(24.0);
            v["position_y"] = serde_json::json!(-12.0);
        }
        _ => {}
    }
}

/// Each registered effect the effect itself scales, its pixel sizes set
/// 70 % of the way up their sliders (a whole radius to at least 4, so a
/// half of it is still a radius), its other values the sample's, made
/// visible on the chart where they aren't.
fn cases() -> Vec<(&'static str, BuiltinEffect)> {
    effect_types()
        .filter(|t| t.desc.params.iter().any(|p| p.scaling.by_the_effect()))
        .map(|t| {
            let mut v = serde_json::to_value((t.sample)()).unwrap();
            for p in t.desc.params.iter().filter(|p| p.scaling.by_the_effect()) {
                let (lo, hi) = p.soft.or(p.range).unwrap_or((0.0, 10.0));
                let at = lo + 0.7 * (hi - lo);
                let slot = match p.index {
                    Some(i) => &mut v[p.field][i],
                    None => &mut v[p.field],
                };
                *slot = if p.kind == ParamKind::Whole { serde_json::json!((at.round() as u64).max(4)) } else { serde_json::json!(at) };
            }
            visible(t.tag, &mut v);
            (t.tag, serde_json::from_value(v).unwrap())
        })
        .collect()
}

#[test]
fn every_effect_looks_the_same_at_half_scale() {
    let cases = cases();
    assert!(cases.len() >= 14, "only {} effects scale themselves", cases.len());
    let mut failures = Vec::new();
    for gpu in [false, true] {
        // What a 2x2 average leaves against a half-size render with no
        // effect at all: the floor every case stands on.
        let none = BuiltinEffect::Gamma { gamma: 1.0 };
        let floor = mean_diff(&render(&none, (W, H), W / 2, H / 2, gpu), &shrink(&render(&none, (W, H), W, H, gpu)));
        for (name, effect) in &cases {
            let want = shrink(&render(effect, (W, H), W, H, gpu));
            let scaled = render(effect, (W, H), W / 2, H / 2, gpu);
            let unscaled = render(effect, (W / 2, H / 2), W / 2, H / 2, gpu);
            let (new, old) = (mean_diff(&scaled, &want) - floor, mean_diff(&unscaled, &want) - floor);
            eprintln!("{name} (gpu {gpu}): scaled {new:.5}, in its own pixels {old:.5}, over a floor of {floor:.5}");
            // A feather acts on the crop's edges only: a small share of the
            // picture, but plainly there.
            if old <= 0.0005 {
                failures.push(format!("{name} (gpu {gpu}): the effect doesn't depend on the render size here ({old} over the floor)"));
            } else if !(new < old * 0.75 && new < 0.02) {
                failures.push(format!("{name} (gpu {gpu}): the half render isn't scaled: {new} against {old} in its own pixels"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// G3: a masked blur at half scale, its window's feather and the blur's
/// radius both scaled: a picture-truth clip whose grade publishes a
/// feathered window that masks its blur (node-graph M2 step 9c-2).
#[test]
fn a_masked_blur_at_half_scale() {
    use klipp_graph_format::lifted::types as lt;
    use klipp_graph_format::path::{NodePath, Root};
    use klipp_nodes::{Edge, Node, NodeGraph, NodeKind, NodeParams};
    let masked = |seq: (u32, u32), w: u32, h: u32, gpu: bool| {
        let node = |kind: NodeKind, params: NodeParams| Node { id: klipp_nodes::NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] };
        let masks = klipp_core::MaskStack { masks: vec![klipp_core::Mask { shape: klipp_core::MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.3 }, feather: 0.15, ..klipp_core::Mask::default() }] };
        let (source, output) = (node(NodeKind::Source, NodeParams::default()), node(NodeKind::Output, NodeParams::default()));
        let window = node(NodeKind::Mask, NodeParams { masks: Some(masks), ..Default::default() });
        let graph = NodeGraph {
            output_node_id: output.id,
            edges: vec![
                Edge { from_node: source.id, from_port: 0, to_node: output.id, to_port: 0 },
                Edge { from_node: source.id, from_port: 0, to_node: window.id, to_port: 0 },
            ],
            nodes: vec![source, output, window.clone()],
            node_positions: Default::default(),
            working_space_override: None,
            frames: vec![],
            sticky_notes: vec![],
        };
        let mut clip = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
        klipp_timeline::GradeAccess::set_grade(&mut clip, graph);
        clip = TestClip::of(clip).effect(BuiltinEffect::GaussianBlur { radius: 6.0 }).build();
        let p = clip.picture_v2().unwrap().picture.clone();
        let group = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).unwrap().id;
        let blur = p.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::types::LEGACY_EFFECT).unwrap().id;
        let at = |id| NodePath::new(Root::Clip { clip: clip.id.0 }, [id]);
        clip.apply_picture_ops(&[
            klipp_graph_lift::ops::GraphOp::PublishOutput { group: at(group), endpoint: klipp_graph_format::PortBinding { node: window.id, port: "out".into() }, key: "window".into(), label: "Window".into() },
            klipp_graph_lift::ops::GraphOp::Connect { from: at(group), from_port: "window".into(), to: at(blur), to_port: "mask".into() },
        ])
        .unwrap();
        let mut s = Sequence::new_default();
        s.resolution = Resolution { width: seq.0, height: seq.1 };
        let mut track = Track::new("V", TrackKind::Video);
        track.clips = vec![clip];
        s.tracks = vec![track];
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(gpu);
        r.render(&s, tc(1.0), w, h, &mut Chart, RenderOptions::default())
    };
    for gpu in [false, true] {
        let none = BuiltinEffect::Gamma { gamma: 1.0 };
        let floor = mean_diff(&render(&none, (W, H), W / 2, H / 2, gpu), &shrink(&render(&none, (W, H), W, H, gpu)));
        let want = shrink(&masked((W, H), W, H, gpu));
        let scaled = masked((W, H), W / 2, H / 2, gpu);
        let unscaled = masked((W / 2, H / 2), W / 2, H / 2, gpu);
        let (new, old) = (mean_diff(&scaled, &want) - floor, mean_diff(&unscaled, &want) - floor);
        eprintln!("masked blur (gpu {gpu}): scaled {new:.5}, in its own pixels {old:.5}, over a floor of {floor:.5}");
        assert!(old > 0.0005, "gpu {gpu}: the masked blur doesn't depend on the render size here ({old} over the floor)");
        assert!(new < old * 0.75 && new < 0.02, "gpu {gpu}: the half render isn't scaled: {new} against {old} in its own pixels");
    }
}
