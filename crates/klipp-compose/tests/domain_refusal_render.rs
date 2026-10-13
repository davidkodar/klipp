//! #1867: a render whose run the executor refuses for its colour domains
//! says so, a failing decline naming the node, on the CPU and the GPU; an
//! SDR clip's picture is display-referred, so its run counts it so, and a
//! scene-referred (HDR) clip's grade reads scene light.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, Timecode};
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

const W: u32 = 32;
const H: u32 = 18;

fn tc(s: f64) -> Timecode {
    RationalTime::from_secs_f64(s, 24)
}

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
}

fn step() -> NodeParams {
    NodeParams { scene_to_display: Some(klipp_color::hdr::SceneToDisplay { transfer: klipp_color::hdr::HdrTransfer::Pq, peak_nits: None, luma: [0.2126, 0.7152, 0.0722] }), ..Default::default() }
}

/// Source -> display step -> Blend -> Output, the Blend's second picture
/// the Source's own: the picture mixed with itself displayed. And the
/// display step's and the Blend's ids.
fn mixed_with_its_display() -> (NodeGraph, NodeId, NodeId) {
    let (s, d, b, o) = (node(NodeKind::Source, NodeParams::default()), node(NodeKind::SceneToDisplay, step()), node(NodeKind::Blend, NodeParams { mix: Some(0.5), ..Default::default() }), node(NodeKind::Output, NodeParams::default()));
    let e = |a: &Node, b: &Node, port| Edge { from_node: a.id, from_port: 0, to_node: b.id, to_port: port };
    let edges = vec![e(&s, &d, 0), e(&d, &b, 0), e(&s, &b, 1), e(&b, &o, 0)];
    let (dn, bn) = (d.id, b.id);
    (NodeGraph { output_node_id: o.id, nodes: vec![s, d, b, o], edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }, dn, bn)
}

/// A grey SDR picture, or HDR (PQ) codes when `hdr`.
struct Source {
    hdr: bool,
}

impl FrameSource for Source {
    fn frame(&mut self, _: &Clip, _: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let pixels = if self.hdr {
            let codes = klipp_color::hdr::SceneCodes::Packed10((0..w * h).map(|i| klipp_color::hdr::pack_rgb10(300 + i % 200, 400, 500)).collect());
            SourcePixels::SceneReferred { codes: Arc::new(codes), transfer: klipp_color::hdr::HdrTransfer::Pq, source_to_working: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] }
        } else {
            SourcePixels::Linear(Arc::new([0.2f32, 0.3, 0.4, 1.0].repeat((w * h) as usize)))
        };
        Some(SourceFrame { pixels, width: w, height: h, pts: None })
    }
}

fn sequence(grade: NodeGraph) -> Sequence {
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    c.set_grade(grade);
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut t = Track::new("V1", TrackKind::Video);
    t.clips = vec![c];
    seq.tracks = vec![t];
    seq
}

fn render(seq: &Sequence, gpu: bool, hdr: bool) -> klipp_compose::Inspection {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.render_inspected(seq, tc(1.0), W, H, &mut Source { hdr }, RenderOptions::default(), Inspect::default()).1
}

/// An SDR clip's picture is display-referred: a display step in its grade
/// would turn it twice. Its run is refused, and the render says so, a
/// failing decline naming the display step, on both routes.
#[test]
fn a_refused_run_records_a_failing_decline_naming_the_node() {
    let (grade, display, _) = mixed_with_its_display();
    let seq = sequence(grade);
    for gpu in [false, true] {
        let found = render(&seq, gpu, false);
        let d = found.declines.iter().find(|d| d.node == display).unwrap_or_else(|| panic!("gpu {gpu}: no decline for the display step: {:?}", found.declines));
        assert!(d.outcome.fails() && d.kind == NodeKind::SceneToDisplay && d.why.contains("display-referred already"), "gpu {gpu}: {d:?}");
        assert_eq!(d.clip, Some(seq.tracks[0].clips[0].id), "named against its clip");
    }
}

/// An HDR clip's grade reads scene light: the display step runs, and the
/// Blend mixing its scene light with the displayed picture is refused,
/// said by a failing decline naming the Blend.
#[test]
fn an_hdr_clip_mixing_scene_and_display_light_is_refused_with_a_decline() {
    let (grade, display, blend) = mixed_with_its_display();
    let seq = sequence(grade);
    for gpu in [false, true] {
        let found = render(&seq, gpu, true);
        assert!(found.declines.iter().all(|d| d.node != display), "gpu {gpu}: the display step takes scene light: {:?}", found.declines);
        let d = found.declines.iter().find(|d| d.node == blend).unwrap_or_else(|| panic!("gpu {gpu}: no decline for the Blend: {:?}", found.declines));
        assert!(d.outcome.fails() && d.why.contains("mix a scene-referred picture with a display-referred one"), "gpu {gpu}: {d:?}");
    }
}

/// Control: an SDR clip's grade with no display step mixes its picture
/// with itself: no refusal, no decline.
#[test]
fn an_sdr_grade_that_mixes_display_referred_pictures_runs() {
    let (mut grade, display, _) = mixed_with_its_display();
    grade.nodes.retain(|n| n.id != display);
    let (source, blend) = (grade.nodes[0].id, grade.nodes[1].id);
    grade.edges.retain(|e| e.from_node != display && e.to_node != display);
    grade.edges.push(Edge { from_node: source, from_port: 0, to_node: blend, to_port: 0 });
    let seq = sequence(grade);
    for gpu in [false, true] {
        let found = render(&seq, gpu, false);
        assert!(found.declines.is_empty(), "gpu {gpu}: {:?}", found.declines);
    }
}
