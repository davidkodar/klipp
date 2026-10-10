//! A clip's OFX plug-in runs as a node of its own after the clip's grade
//! (M4 step 3), where the OFX stage it replaced ran: a graded clip with an
//! inverting plug-in draws its graded frame inverted, never its source
//! inverted and then graded.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::RationalTime;
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

const W: u32 = 32;
const H: u32 = 18;

/// A ramp, so a grade and an inversion in either order give different
/// pictures.
struct Ramp;

impl FrameSource for Ramp {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let v: Vec<f32> = (0..w * h).flat_map(|i| { let x = (i % w) as f32 / w as f32; [x * 0.8, 0.3 + x * 0.2, 0.6 - x * 0.5, 1.0] }).collect();
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

/// A media clip graded by one exposure node, with `invert` its plug-in.
fn graded(invert: bool) -> Sequence {
    let node = |kind: NodeKind, params: NodeParams| Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] };
    let nodes = vec![
        node(NodeKind::Source, NodeParams::default()),
        node(NodeKind::Exposure, NodeParams { exposure: Some(0.8), ..Default::default() }),
        node(NodeKind::Output, NodeParams::default()),
    ];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    let graph = NodeGraph { output_node_id: nodes[2].id, nodes, edges, ..Default::default() };
    let mut clip = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    clip.set_grade(graph);
    if invert {
        clip.set_ofx_effects(vec![klipp_plugin::AppliedEffect { plugin_name: "com.klipp.test.Invert".into(), enabled: true, ..Default::default() }]);
    }
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    seq
}

/// The frame of `seq`, as display bytes, from a renderer with the invert
/// plug-in, on the CPU.
fn drawn(seq: &Sequence) -> Vec<u8> {
    let mut host = klipp_plugin::OfxHost::new();
    let at = host.register_raw_plugin(klipp_plugin::test_plugin::create_test_plugin_raw());
    host.initialise_plugin(at).expect("the invert plug-in loads");
    let mut r = Renderer::new(host, klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    klipp_effects::linear_f32_rgba_to_srgb_u8(&r.render(seq, tc(1.0), W, H, &mut Ramp, RenderOptions::default()))
}

#[test]
fn an_ofx_plugin_runs_after_the_grade() {
    let graded_only = drawn(&graded(false));
    let with = drawn(&graded(true));
    // The graded frame inverted, in the 8-bit sRGB the plug-in takes.
    let after: Vec<u8> = graded_only.chunks_exact(4).flat_map(|p| [255 - p[0], 255 - p[1], 255 - p[2], p[3]]).collect();
    let worst = with.iter().zip(&after).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
    assert!(with.len() == after.len() && worst <= 1, "the plug-in didn't invert the graded frame: {worst} levels from it");
    // The order is what the check above pins: the grade brightens, and a
    // brightening and an inversion don't commute, so the source inverted
    // and then graded is another picture. That the plug-in ran at all:
    assert!(graded_only.iter().zip(&with).any(|(a, b)| a.abs_diff(*b) > 8), "the plug-in changed nothing");
}
