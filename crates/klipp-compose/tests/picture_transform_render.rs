//! A Picture Transform node (#1746) moves, scales and turns the picture
//! inside the clip's frame where it sits, the same on the GPU (the
//! viewer's route) and the CPU, as the Transform effect's kernels draw it.
//! It is never the clip's own move: with the clip moved too, the placement
//! still places the clip, after the node, once.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::RationalTime;
use klipp_effects::BuiltinEffect as E;
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

fn moved() -> E {
    E::PictureTransform { position_x: 10.0, position_y: -6.0, scale_x: 0.7, scale_y: 0.6, rotation: 12.0, anchor_x: 0.5, anchor_y: 0.5 }
}

/// A clip of the chart with `fx` added to its picture as the Picture view
/// adds nodes, after the display step.
fn clip(fx: &[E]) -> Clip {
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let picture = c.picture_v2().unwrap().picture.clone();
    let mut after = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let mut ops = Vec::new();
    for e in fx {
        let payload = serde_json::to_value(e).unwrap();
        let node: klipp_graph_format::Node = serde_json::from_value(serde_json::json!({
            "id": klipp_graph_format::NodeId::new_v4(),
            "type_id": klipp_graph_lift::names::effect_node_type(payload["type"].as_str().unwrap()),
            "legacy": { "payload": payload },
        }))
        .unwrap();
        let id = node.id;
        ops.push(GraphOp::AddNode { after: NodePath::new(Root::Clip { clip: c.id.0 }, [after]), node: Box::new(node) });
        after = id;
    }
    if !ops.is_empty() {
        c.apply_picture_ops(&ops).unwrap();
    }
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

/// The worst difference in 8-bit sRGB levels, colour channels: the frame
/// is composited over black, where the kernels leave the margin clear.
fn worst_levels(a: &[f32], b: &[f32]) -> u8 {
    let (a, b) = (klipp_effects::linear_f32_rgba_to_srgb_u8(a), klipp_effects::linear_f32_rgba_to_srgb_u8(b));
    a.iter().zip(&b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| x.abs_diff(*y)).max().unwrap_or(0)
}

fn gpu() -> bool {
    let ok = klipp_nodes::GraphExecutor::new().gpu_pipeline_or_init().is_some();
    assert!(ok || !required(), "required, but there is no GPU");
    ok
}

#[test]
fn the_node_draws_the_transform_kernels_on_the_gpu_and_the_cpu() {
    let (plain, _) = render(false, &clip(&[]));
    let flat = moved().picture_transform_as_transform().unwrap();
    // The kernels' picture, composited over the black the frame sits on.
    let want: Vec<f32> = klipp_effects::apply_effects(&[flat], plain.clone(), W, H).chunks_exact(4).flat_map(|p| [p[0] * p[3], p[1] * p[3], p[2] * p[3], 1.0]).collect();
    assert!(worst_levels(&want, &plain) > 20, "the node moves the picture");
    let c = clip(&[moved()]);
    let (cpu, found) = render(false, &c);
    assert!(found.declines.is_empty(), "{:?}", found.declines);
    let worst = worst_levels(&cpu, &want);
    eprintln!("CPU against the kernels: worst {worst} levels");
    assert!(worst <= 1, "the CPU draws {worst} levels from the kernels");
    if !gpu() {
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let (on_gpu, found) = render(true, &c);
    assert_eq!((found.stats.effects_on_cpu, found.stats.effects_on_cpu_kernels), (0, 0), "the GPU ran it");
    let worst = worst_levels(&on_gpu, &want);
    eprintln!("GPU against the kernels: worst {worst} levels");
    assert!(worst <= 1, "the GPU draws {worst} levels from the kernels");
}

/// With the clip moved by its own placement, the node is drawn where it
/// sits and the placement is applied once, after it: the effects the
/// renderer resolves hold both, the node's unchanged.
#[test]
fn the_node_is_never_the_clips_own_move() {
    let c = clip(&[moved()]);
    // The clip moved 20 pixels right, as its transform resolves.
    let own = Some((20.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5));
    let fx = klipp_compose::resolve::resolve_effects(&c, &c.picture_v2().unwrap().picture, tc(1.0), 24.0, own, None).unwrap();
    let at = fx.iter().position(|e| matches!(e, E::PictureTransform { .. })).expect("the node is drawn");
    assert_eq!(fx[at], moved(), "the node keeps its own values");
    let placed: Vec<_> = fx.iter().enumerate().filter(|(_, e)| matches!(e, E::Transform { .. })).map(|(i, _)| i).collect();
    assert_eq!(placed.len(), 1, "the clip is placed once: {fx:?}");
    assert!(placed[0] > at, "after the node, which sits before it");
    // And the list the CPU route reads agrees.
    let (list, _) = klipp_compose::render::clip_effects(&c, tc(1.0), 24.0, own, None);
    assert_eq!(list.iter().filter(|e| matches!(e, E::PictureTransform { .. })).count(), 1);
}
