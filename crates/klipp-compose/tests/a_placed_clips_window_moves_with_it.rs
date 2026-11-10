//! A grade's window cuts the clip's own picture, before the clip is
//! placed: on a clip at half its size, moved 200 pixels right, a circle at
//! its centre corrects the frame where the placed picture's centre is, at
//! half its radius. What the viewer's mask overlay draws over (it has to
//! follow the placement too).

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::RationalTime;
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
use klipp_timeline::{Clip, GradeAccess, Sequence, Track, TrackKind};

const W: u32 = 640;
const H: u32 = 360;

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

struct Grey;
impl FrameSource for Grey {
    fn frame(&mut self, _: &Clip, _: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let v: Vec<f32> = (0..w * h).flat_map(|_| [0.1, 0.1, 0.1, 1.0]).collect();
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }
}

#[test]
fn a_window_on_a_placed_clip_corrects_its_placed_picture() {
    let mut c = Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    let mut g = NodeGraph::default();
    let corr = g.ensure_corrector();
    g.nodes.iter_mut().find(|n| n.id == corr).unwrap().params.exposure = Some(2.0);
    let masks = klipp_core::MaskStack { masks: vec![klipp_core::Mask { shape: klipp_core::MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.1 }, feather: 0.0, ..klipp_core::Mask::default() }] };
    let mask = Node { id: NodeId::new_v4(), kind: NodeKind::Mask, label: "Window".into(), enabled: true, params: NodeParams { masks: Some(masks), ..Default::default() }, keyframes: Default::default(), output_link: None, input_links: vec![] };
    g.edges.push(Edge { from_node: mask.id, from_port: 0, to_node: corr, to_port: 1 });
    g.nodes.insert(0, mask);
    c.set_grade(g);
    let c = klipp_timeline::test_clip::TestClip::of(c)
        .effect(klipp_effects::BuiltinEffect::Transform {
            position_x: 200.0, position_y: 0.0, scale_x: 0.5, scale_y: 0.5, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
        })
        .build();
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![c];
    seq.tracks = vec![v1];
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    let px = r.render(&seq, tc(1.0), W, H, &mut Grey, RenderOptions::default());
    // The corrected pixels: brighter than the grey the placed picture is.
    let lit: Vec<(f32, f32)> = (0..H).flat_map(|y| (0..W).map(move |x| (x, y))).filter(|&(x, y)| px[((y * W + x) * 4) as usize] > 0.2).map(|(x, y)| (x as f32 + 0.5, y as f32 + 0.5)).collect();
    assert!(!lit.is_empty(), "the window corrects something");
    let n = lit.len() as f32;
    let (cx, cy) = (lit.iter().map(|p| p.0).sum::<f32>() / n, lit.iter().map(|p| p.1).sum::<f32>() / n);
    let width = lit.iter().map(|p| p.0).fold(f32::MIN, f32::max) - lit.iter().map(|p| p.0).fold(f32::MAX, f32::min);
    eprintln!("lit {n} px, centre ({cx:.1}, {cy:.1}), across {width:.1} px");
    // Moved 200 of the sequence's pixels right of the frame's centre.
    let want_x = W as f32 / 2.0 + 200.0;
    assert!((cx - want_x).abs() < 2.0 && (cy - H as f32 / 2.0).abs() < 2.0, "the window's centre at ({cx}, {cy}), the placed picture's at ({want_x}, {})", H / 2);
    // Half the radius: 0.1 of the picture's width, at half its size, across.
    let want_w = 2.0 * 0.1 * W as f32 * 0.5;
    assert!((width - want_w).abs() < 4.0, "{width} px across, the placed window's {want_w}");
}
