//! Two gated corrections keyed off the clip's frame, as a lifted grade
//! keys them (M3 step 17b), draw on the GPU what they draw on the CPU,
//! through the renderer the viewer and the export share.

use std::sync::Arc;
use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime};
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

const W: u32 = 96;
const H: u32 = 54;
fn tc(s: f64) -> RationalTime { RationalTime::from_secs_f64(s, 24) }

struct Chart;
impl FrameSource for Chart {
    fn frame(&mut self, _: &Clip, _: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let mut pixels = Vec::new();
        for y in 0..h { for x in 0..w {
            let t = x as f32 / w as f32;
            pixels.extend_from_slice(&[0.05 + 0.7 * t, 0.1 + 0.4 * y as f32 / h as f32, 0.7 - 0.6 * t, 1.0]);
        }}
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(pixels)), width: w, height: h, pts: None })
    }
}

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() }
}

/// A clip graded Source → C1 → C2, each correction gated by a qualifier
/// keying the source.
fn sequence() -> Sequence {
    let mut c = Clip { kind: ClipKind::Media { media_id: MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let window = |hue: f32| klipp_core::QualifierSettings { enabled: true, hue_center: hue, hue_width: 80.0, hue_soft: 20.0, sat_low: 0.05, sat_high: 1.0, sat_soft: 0.05, lum_low: 0.0, lum_high: 1.0, lum_soft: 0.05, ..Default::default() };
    let src = node(NodeKind::Source, NodeParams::default());
    let q1 = node(NodeKind::Qualifier, NodeParams { qualifier: Some(window(20.0)), ..Default::default() });
    let c1 = node(NodeKind::Corrector, NodeParams { temperature: Some(-40.0), ..Default::default() });
    let q2 = node(NodeKind::Qualifier, NodeParams { qualifier: Some(window(220.0)), ..Default::default() });
    let c2 = node(NodeKind::Corrector, NodeParams { saturation: Some(0.3), ..Default::default() });
    let out = node(NodeKind::Output, NodeParams::default());
    let e = |a: &Node, b: &Node, port: u8| Edge { from_node: a.id, from_port: 0, to_node: b.id, to_port: port };
    let edges = vec![e(&src, &c1, 0), e(&c1, &c2, 0), e(&c2, &out, 0), e(&src, &q1, 0), e(&q1, &c1, 1), e(&src, &q2, 0), e(&q2, &c2, 1)];
    let output_node_id = out.id;
    c.set_grade(NodeGraph { nodes: vec![src, q1, c1, q2, c2, out], edges, output_node_id, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] });
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![c];
    seq.tracks = vec![track];
    seq
}

fn render(seq: &Sequence, gpu: bool) -> Vec<f32> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.render_inspected(seq, tc(1.0), W, H, &mut Chart, RenderOptions::default(), Inspect::default()).0
}

#[test]
fn two_gates_keyed_off_the_frame_draw_the_cpus_picture() {
    let gpu = klipp_nodes::GraphExecutor::new().gpu_pipeline_or_init().is_some();
    assert!(gpu || std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required GPU unavailable");
    if !gpu {
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let seq = sequence();
    let (cpu, on_gpu) = (render(&seq, false), render(&seq, true));
    let worst = cpu.iter().zip(&on_gpu).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    eprintln!("two gates keyed off the frame, GPU vs CPU: worst {worst}");
    assert!(worst < 2.0 / 255.0, "the GPU draws {worst} away from the CPU");
    // And the gates do something: without the corrections the picture differs.
    let mut plain = seq.clone();
    plain.tracks[0].clips[0].edit_grade(|g| g.nodes.iter_mut().filter(|n| n.kind == NodeKind::Corrector).for_each(|n| n.enabled = false));
    let flat = render(&plain, true);
    assert!(flat.iter().zip(&on_gpu).any(|(a, b)| (a - b).abs() > 0.02), "the gated corrections change the picture");
}
