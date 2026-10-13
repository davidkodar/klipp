//! A Merge lays the picture wired into its `fg` port over the clip's
//! (#1746): a Generator beside the picture's chain, reading the clip's
//! source, its picture laid over by the merge's blend mode and opacity,
//! weighed by its alpha. Drawn the same on the GPU (the viewer's route)
//! and the CPU, as the blend's formula gives; with nothing wired, the
//! picture passes.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::RationalTime;
use klipp_effects::{BlendMode, BuiltinEffect as E};
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 96;
const H: u32 = 54;

struct Chart;

impl FrameSource for Chart {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
                v.extend_from_slice(&[0.1 + 0.8 * fx, 0.1 + 0.6 * fy, 0.6 * (1.0 - fx), 1.0]);
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

/// A gradient from opaque blue to a half-clear warm white.
fn gradient() -> E {
    E::Generator { shape: 1, color_a: [20, 40, 220, 255], color_b: [250, 230, 200, 128], angle: 20.0, center_x: 0.5, center_y: 0.5, radius: 0.5 }
}

/// An effect node for `e`, as the Picture view makes one.
fn effect_node(e: &E) -> klipp_graph_format::Node {
    let payload = serde_json::to_value(e).unwrap();
    serde_json::from_value(serde_json::json!({
        "id": klipp_graph_format::NodeId::new_v4(),
        "type_id": klipp_graph_lift::names::effect_node_type(payload["type"].as_str().unwrap()),
        "legacy": { "payload": payload },
    }))
    .unwrap()
}

/// The chart clip with `merge` after its display step and, when `fg`, a
/// generator beside the chain reading its source, wired into the merge's
/// `fg`: as the Picture view's Sources menu makes it.
fn clip(merge: Option<E>, fg: Option<E>) -> Clip {
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let Some(merge) = merge else { return c };
    let picture = c.picture_v2().unwrap().picture.clone();
    let at = |id| NodePath::new(Root::Clip { clip: c.id.0 }, [id]);
    let display = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let source = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
    let m = effect_node(&merge);
    let mut ops = vec![GraphOp::AddNode { after: at(display), node: Box::new(m.clone()) }];
    if let Some(fg) = fg {
        let g = effect_node(&fg);
        ops.extend([
            GraphOp::AddNodeBeside { at: at(source), node: Box::new(g.clone()) },
            GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(g.id), to_port: "in".into() },
            GraphOp::Connect { from: at(g.id), from_port: "out".into(), to: at(m.id), to_port: "fg".into() },
        ]);
    }
    c.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{e}"));
    c
}

fn sequence(c: Clip) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![c];
    seq.tracks = vec![track];
    seq
}

fn render(gpu: bool, c: &Clip) -> (Vec<f32>, klipp_compose::Inspection) {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.render_inspected(&sequence(c.clone()), tc(1.0), W, H, &mut Chart, RenderOptions::default(), Inspect::default())
}

fn worst_levels(a: &[f32], b: &[f32]) -> u8 {
    let (a, b) = (klipp_effects::linear_f32_rgba_to_srgb_u8(a), klipp_effects::linear_f32_rgba_to_srgb_u8(b));
    a.iter().zip(&b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| x.abs_diff(*y)).max().unwrap_or(0)
}

fn gpu() -> bool {
    let ok = klipp_nodes::GraphExecutor::new().gpu_pipeline_or_init().is_some();
    assert!(ok || !required(), "required, but there is no GPU");
    ok
}

/// The merge's picture by its formula: the generator's over the plain
/// one, by `mode`, `opacity` times the generator's alpha.
fn oracle(mode: BlendMode, opacity: f32) -> Vec<f32> {
    let (plain, _) = render(false, &clip(None, None));
    let fg = klipp_effects::apply_effects(&[gradient()], plain.clone(), W, H);
    plain
        .chunks_exact(4)
        .zip(fg.chunks_exact(4))
        .flat_map(|(b, t)| {
            let k = opacity * t[3].clamp(0.0, 1.0);
            let m = |i: usize| match mode {
                BlendMode::Multiply => t[i] * b[i],
                _ => t[i],
            };
            [b[0] + (m(0) - b[0]) * k, b[1] + (m(1) - b[1]) * k, b[2] + (m(2) - b[2]) * k, b[3]]
        })
        .collect()
}

#[test]
fn a_merge_lays_its_picture_over_the_same_on_the_gpu_and_the_cpu() {
    let (plain, _) = render(false, &clip(None, None));
    for (mode, opacity) in [(BlendMode::Normal, 0.7), (BlendMode::Multiply, 1.0)] {
        let c = clip(Some(E::Merge { blend_mode: mode, opacity }), Some(gradient()));
        assert!(c.is_picture_truth(), "a picture wired from beside the chain is the picture's");
        let want = oracle(mode, opacity);
        assert!(worst_levels(&want, &plain) > 10, "{mode:?}: the merge changes the picture");
        let (cpu, found) = render(false, &c);
        assert!(found.declines.is_empty(), "{mode:?}: {:?}", found.declines);
        let worst = worst_levels(&cpu, &want);
        eprintln!("{mode:?}: CPU against the formula: worst {worst} levels");
        assert!(worst <= 1, "{mode:?}: the CPU draws {worst} levels from the formula");
        if !gpu() {
            eprintln!("UNVERIFIED: no GPU");
            continue;
        }
        let (on_gpu, _) = render(true, &c);
        let worst = worst_levels(&on_gpu, &cpu);
        eprintln!("{mode:?}: GPU against CPU: worst {worst} levels");
        assert!(worst <= 1, "{mode:?}: the GPU draws {worst} levels from the CPU");
    }
}

#[test]
fn a_merge_with_nothing_wired_passes_the_picture() {
    let (plain, _) = render(false, &clip(None, None));
    let c = clip(Some(E::Merge { blend_mode: BlendMode::Screen, opacity: 1.0 }), None);
    for gpu_on in [false, true] {
        if gpu_on && !gpu() {
            continue;
        }
        let (out, found) = render(gpu_on, &c);
        assert!(found.declines.is_empty());
        assert_eq!(worst_levels(&out, &plain), 0, "gpu {gpu_on}");
    }
}
