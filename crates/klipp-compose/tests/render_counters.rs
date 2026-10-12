//! The GPU stack's counters as data: a layer it had to render on the CPU
//! and upload is counted in `RenderStats::cpu_layers`, and why it
//! declined a frame is `Renderer::last_decline`, so a test checks "no new
//! CPU layer" and "no new decline" without reading the log.
//!
//! A missing GPU is reported as unverified; with `KLIPP_TEST_REQUIRE_GPU=1`
//! it fails instead.

use klipp_timeline::GradeAccess;
use std::collections::HashMap;
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ClipId, RationalTime, Timecode};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};
use klipp_timeline::test_clip::TestClip;

const W: u32 = 320;
const H: u32 = 180;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn clip(kind: ClipKind, from: f64, to: f64) -> Clip {
    Clip { kind, ..Clip::gap(tc(from), tc(to)) }
}

#[derive(Default)]
struct Flat(HashMap<ClipId, Arc<Vec<f32>>>);

impl FrameSource for Flat {
    fn frame(&mut self, clip: &Clip, _source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        self.0.get(&clip.id).map(|p| SourceFrame { pixels: SourcePixels::Linear(Arc::clone(p)), width, height, pts: None })
    }
}

fn picture(seed: f32) -> Arc<Vec<f32>> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let fx = x as f32 / (W - 1) as f32;
            let fy = y as f32 / (H - 1) as f32;
            v.extend_from_slice(&[(fx + seed).fract() * 0.8, fy * 0.6, ((fx + fy) * 0.5 + seed).fract() * 0.7, 1.0]);
        }
    }
    Arc::new(v)
}

fn renderer() -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    r
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// A grade that isn't identity: an exposure node.
fn exposure_grade() -> klipp_nodes::graph::NodeGraph {
    use klipp_nodes::{graph::{Edge, Node, NodeGraph, NodeId, NodePos}, kinds::NodeKind, params::NodeParams};
    let node = |kind: NodeKind, params: NodeParams| Node {
        id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![],
    };
    let nodes = vec![
        node(NodeKind::Source, NodeParams::default()),
        node(NodeKind::Exposure, NodeParams { exposure: Some(-0.5), ..Default::default() }),
        node(NodeKind::Output, NodeParams::default()),
    ];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
    NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

/// Two media layers, one with an opacity, and a generator over them,
/// graded when `graded_generator`: the stack seeds an identity-graded
/// generator on the GPU, and renders a graded one on the CPU and uploads it.
fn three_layers(graded_generator: bool) -> (Sequence, Flat) {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut gen = Track::new("Gen", TrackKind::Video);
    let mut g = clip(ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color: [255, 80, 20, 255] }), 0.0, 5.0);
    g.blend_mode = klipp_core::BlendMode::Multiply;
    if graded_generator {
        g.set_grade(exposure_grade());
    }
    gen.clips = vec![g];
    let mut top = Track::new("V2", TrackKind::Video);
    let mut t = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 10.0);
    t = TestClip::of(t).effects([klipp_effects::BuiltinEffect::Opacity { value: 0.5 }]).build();
    let mut base = Track::new("V1", TrackKind::Video);
    let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 10.0);
    let mut src = Flat::default();
    src.0.insert(t.id, picture(0.3));
    src.0.insert(b.id, picture(0.0));
    top.clips = vec![t];
    base.clips = vec![b];
    seq.tracks = vec![gen, top, base];
    (seq, src)
}

#[test]
fn a_layer_rendered_on_the_cpu_is_counted() {
    let mut r = renderer();
    let (seq, mut src) = three_layers(false);
    let Some((_, stats)) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) else {
        assert!(!required(), "required but the GPU stack declined: {:?}", r.last_decline());
        eprintln!("UNVERIFIED: the GPU stack declined ({:?})", r.last_decline());
        return;
    };
    assert_eq!(stats.cpu_layers, 0, "every layer has a GPU seed");
    assert_eq!(r.last_decline(), None);

    let (seq, mut src) = three_layers(true);
    let (_, stats) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()).expect("the stack takes a graded generator");
    assert_eq!(stats.cpu_layers, 1, "the graded generator is rendered on the CPU and uploaded");

    // Counted per frame, not accumulated across frames.
    let (_, stats) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()).unwrap();
    assert_eq!(stats.cpu_layers, 1);
}

#[test]
fn a_decline_says_why_and_a_taken_frame_clears_it() {
    let mut r = renderer();
    let (seq, mut src) = three_layers(false);
    let hdr = RenderOptions { hdr_peak_nits: Some(1000.0), ..Default::default() };
    assert!(r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, hdr).is_none());
    let why = r.last_decline().expect("a declined frame says why");
    if r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()).is_none() {
        assert!(!required(), "required but the GPU stack declined: {:?}", r.last_decline());
        eprintln!("UNVERIFIED: the GPU stack declined ({:?})", r.last_decline());
        return;
    }
    assert_eq!(why, "the viewer's HDR preview of a stack");
    assert_eq!(r.last_decline(), None, "a frame the stack took clears the last decline");

    // The export path says why too.
    // A PQ encode for a render asked for SDR: a wiring mistake it refuses.
    let target = klipp_effects::gpu::Yuv420Target {
        tone_map_op: 0,
        odt_tag: 0,
        scene: None,
        gamut_op: 0,
        hdr_op: 1,
        peak_nits: 1000.0,
        luma_coefs: [0.2627, 0.6780, 0.0593],
        semi_planar: true,
        bits: 10,
        sample_shift: 6,
    };
    assert!(r.render_yuv420_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default(), target).is_none());
    assert_eq!(r.last_decline(), Some("the render and the encode disagree on HDR"));
}
