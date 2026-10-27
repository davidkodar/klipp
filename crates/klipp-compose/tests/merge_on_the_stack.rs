//! A Merge with a picture wired into its `fg` stays on the GPU stack
//! (#2758): a gradient generator and a media item beside the clip's
//! chain, laid over the clip by each of the thirteen blend modes at an
//! opacity below one, on the viewer's display stack. Its frame is the
//! CPU's within a level, the merge drawn on the chain with nothing falling
//! back and nothing read back but the frame the stack hands over. It ran on the CPU, its result read back
//! and sent up again.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, WorkingColorspace};
use klipp_effects::{BlendMode, BuiltinEffect as E};
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;
use klipp_nodes::side_picture::SideMissing;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 96;
const H: u32 = 54;
const ITEM: &str = "2b8f0e51-6d3c-4a7e-9f12-5c4d8e7a1b30";

/// The clip's chart, and the item's picture: stripes, a quarter of them
/// half clear.
struct Sources;

impl FrameSource for Sources {
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

    fn media_frame(&mut self, media: &str, _secs: f64, _working: WorkingColorspace, w: u32, h: u32) -> Result<Vec<f32>, SideMissing> {
        if media != ITEM {
            return Err(SideMissing::File(format!("{media}.mov")));
        }
        Ok((0..w * h)
            .flat_map(|i| {
                let x = i % w;
                let on = (x / 8).is_multiple_of(2);
                [if on { 0.85 } else { 0.05 }, 0.45, 0.25, if (x / 8) % 4 == 3 { 0.5 } else { 1.0 }]
            })
            .collect())
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
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

/// The chart clip with `merge` after its display step, `fg` beside the
/// chain reading its source wired into the merge's `fg`, as the Picture
/// view's Sources menu makes it; a grade on the clip so its picture runs
/// in a plan.
fn clip(merge: Option<E>, fg: &E) -> Clip {
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let Some(merge) = merge else { return c };
    let picture = c.picture_v2().unwrap().picture.clone();
    let at = |id| NodePath::new(Root::Clip { clip: c.id.0 }, [id]);
    let display = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let source = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
    let (m, g) = (effect_node(&merge), effect_node(fg));
    c.apply_picture_ops(&[
        GraphOp::AddNode { after: at(display), node: Box::new(m.clone()) },
        GraphOp::AddNodeBeside { at: at(source), node: Box::new(g.clone()) },
        GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(g.id), to_port: "in".into() },
        GraphOp::Connect { from: at(g.id), from_port: "out".into(), to: at(m.id), to_port: "fg".into() },
    ])
    .unwrap_or_else(|e| panic!("{e}"));
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

fn display(linear: &[f32]) -> Vec<u8> {
    klipp_color::display::display_transform_ws_to(linear, klipp_color::display::ToneMapOp::None, WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb)
}

fn worst(a: &[u8], b: &[u8]) -> i32 {
    a.iter().zip(b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| (*x as i32 - *y as i32).abs()).max().unwrap_or(0)
}

const MODES: [BlendMode; 13] = [
    BlendMode::Normal,
    BlendMode::Add,
    BlendMode::Multiply,
    BlendMode::Screen,
    BlendMode::Overlay,
    BlendMode::SoftLight,
    BlendMode::HardLight,
    BlendMode::Difference,
    BlendMode::Exclusion,
    BlendMode::ColorDodge,
    BlendMode::ColorBurn,
    BlendMode::Darken,
    BlendMode::Lighten,
];

#[test]
fn a_merge_and_its_sources_stay_on_the_gpu_stack() {
    let gradient = E::Generator { shape: 1, color_a: [20, 40, 220, 255], color_b: [250, 230, 200, 128], angle: 20.0, center_x: 0.5, center_y: 0.5, radius: 0.5 };
    let media = E::MediaSource { media: ITEM.into(), offset: 0.0 };
    let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    gpu.set_gpu_enabled(true);
    let mut cpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    cpu.set_gpu_enabled(false);
    let plain = display(&cpu.render_inspected(&sequence(clip(None, &gradient)), tc(1.0), W, H, &mut Sources, RenderOptions::default(), Inspect::default()).0);
    // The stack's own reads back: the display bytes it hands over.
    let reads = klipp_effects::gpu::thread_readback_stats().count;
    let _ = gpu.render_display_u8_on_gpu(&sequence(clip(None, &gradient)), tc(1.0), W, H, &mut Sources, RenderOptions::default());
    let own = klipp_effects::gpu::thread_readback_stats().count - reads;
    for (what, fg) in [("a gradient", &gradient), ("a media item", &media)] {
        for mode in MODES {
            let seq = sequence(clip(Some(E::Merge { blend_mode: mode, opacity: 0.8 }), fg));
            let want = display(&cpu.render_inspected(&seq, tc(1.0), W, H, &mut Sources, RenderOptions::default(), Inspect::default()).0);
            assert!(worst(&want, &plain) > 3, "{what}, {mode:?}: the merge changes the picture");
            let reads = klipp_effects::gpu::thread_readback_stats().count;
            let frame = klipp_effects::residency::frame("test", 1.0);
            let drawn = gpu.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut Sources, RenderOptions::default());
            drop(frame);
            let Some((stack, _)) = drawn else {
                assert!(!required(), "{what}, {mode:?}: the stack declined: {:?}", gpu.last_decline());
                eprintln!("UNVERIFIED: the stack declined ({:?})", gpu.last_decline());
                return;
            };
            let fell = klipp_effects::residency::last_frame();
            assert_eq!(fell.fallback_count(), 0, "{what}, {mode:?}: nothing falls back: {:?}", fell.events);
            assert_eq!(klipp_effects::gpu::thread_readback_stats().count - reads, own, "{what}, {mode:?}: nothing read back but the frame shown");
            let off = worst(&stack, &want);
            assert!(off <= 1, "{what}, {mode:?}: the stack draws {off} levels from the CPU");
        }
    }
}
