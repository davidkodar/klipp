//! A matte wired into a grade's Output is the alpha the graded clip leaves
//! with (#898): over a lower track, the clip shows inside the matte and the
//! track below shows outside it, CPU paths only and GPU (the viewer's and
//! the export's renderer) alike.

use std::collections::HashMap;
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ClipId, Mask, MaskShape, MaskStack, RationalTime, Timecode};
use klipp_nodes::{Edge, NodeGraph, NodeKind};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

const W: u32 = 32;
const H: u32 = 18;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

/// Every media clip a flat grey.
#[derive(Default)]
struct Grey(HashMap<ClipId, f32>);

impl FrameSource for Grey {
    fn frame(&mut self, clip: &Clip, _source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let v = *self.0.get(&clip.id)?;
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new([v, v, v, 1.0].repeat((width * height) as usize))), width, height, pts: None })
    }
}

fn media() -> Clip {
    Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(4.0)) }
}

/// A clip graded by a Corrector, with a hard centred circle into its
/// Output's alpha when `keyed`.
fn graded(keyed: bool) -> Clip {
    let mut c = media();
    let mut g = NodeGraph::default();
    g.ensure_corrector();
    if keyed {
        let source = g.nodes.iter().find(|n| n.kind == NodeKind::Source).expect("the Source").id;
        let circle = Some(MaskStack { masks: vec![Mask { shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.3 }, feather: 0.0, ..Mask::default() }] });
        let id = klipp_nodes::NodeId::new_v4();
        g.nodes.push(klipp_nodes::Node {
            id,
            kind: NodeKind::Mask,
            label: "Mask".into(),
            enabled: true,
            params: klipp_nodes::NodeParams { masks: circle, ..Default::default() },
            keyframes: Default::default(),
            output_link: None,
            input_links: Vec::new(),
        });
        g.edges.push(Edge { from_node: source, from_port: 0, to_node: id, to_port: 0 });
        g.edges.push(Edge { from_node: id, from_port: 0, to_node: g.output_node_id, to_port: 1 });
    }
    c.set_grade(g);
    c
}

/// The pixels at the centre and at a corner, `top` (when any) over a 0.6
/// grey clip, CPU paths only and GPU.
fn render(top: Option<&Clip>) -> [[[f32; 4]; 2]; 2] {
    let below = media();
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = [("V2", top), ("V1", Some(&below))]
        .map(|(name, clip)| {
            let mut t = Track::new(name, TrackKind::Video);
            t.clips = clip.into_iter().cloned().collect();
            t
        })
        .into();
    let mut src = Grey::default();
    if let Some(top) = top {
        src.0.insert(top.id, 0.18);
    }
    src.0.insert(below.id, 0.6);
    [false, true].map(|gpu| {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(gpu);
        let img = r.render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default());
        [(W / 2, H / 2), (1, 1)].map(|(x, y)| {
            let i = ((y * W + x) * 4) as usize;
            [img[i], img[i + 1], img[i + 2], img[i + 3]]
        })
    })
}

fn close(a: [f32; 4], b: [f32; 4]) -> bool {
    (0..4).all(|c| (a[c] - b[c]).abs() <= 2e-3)
}

#[test]
fn a_matte_into_the_grades_output_keys_the_clip_over_the_track_below() {
    let [[plain_centre, plain_corner], _] = render(Some(&graded(false)));
    let [[below_centre, _], _] = render(None);
    assert!(close(plain_centre, plain_corner), "unkeyed, the clip covers the frame");
    assert!(!close(plain_centre, below_centre), "the two tracks must differ to tell them apart");
    for (path, [centre, corner]) in ["CPU", "GPU"].into_iter().zip(render(Some(&graded(true)))) {
        assert!(close(centre, plain_centre), "{path}: inside the matte, the graded clip: {centre:?} vs {plain_centre:?}");
        assert!(close(corner, below_centre), "{path}: outside it, the track below: {corner:?} vs {below_centre:?}");
    }
}

/// A clip whose Output's alpha is one gradient over its whole length, as
/// `opacity_mask` makes it (#2704).
fn faded(shape: MaskShape) -> Clip {
    let mut c = graded(true);
    let mut g = c.grade().clone();
    let mask = g.nodes.iter_mut().find(|n| n.kind == NodeKind::Mask).expect("the matte");
    mask.params.masks = Some(MaskStack { masks: vec![Mask { feather: 0.5, feather_inner: 0.5, ..Mask::new(shape) }] });
    c.set_grade(g);
    c
}

/// A gradient into the alpha fades the clip into the track below across
/// the frame, on the CPU and the GPU alike (#2704).
#[test]
fn a_gradient_into_the_alpha_fades_the_clip_across_the_frame() {
    let [[clip, _], _] = render(Some(&graded(false)));
    let [[below, _], _] = render(None);
    let mix = |a: f32| -> [f32; 4] { std::array::from_fn(|c| clip[c] * a + below[c] * (1.0 - a)) };
    let near = |got: [f32; 4], want: [f32; 4]| (0..3).all(|c| (got[c] - want[c]).abs() <= 0.01);
    // Left to right: the corner, at the start, is the clip; the centre,
    // midway, about half each.
    // Its end past the right edge so the centre column (16 of 0..31) is
    // exactly midway.
    let linear = MaskShape::LinearGradient { start_x: 0.0, start_y: 0.5, end_x: 32.0 / 31.0, end_y: 0.5 };
    for (path, [centre, corner]) in ["CPU", "GPU"].into_iter().zip(render(Some(&faded(linear)))) {
        assert!(near(corner, clip), "{path}: at the start, the clip: {corner:?} vs {clip:?}");
        assert!(near(centre, mix(0.5)), "{path}: midway, half and half: {centre:?} vs {:?}", mix(0.5));
    }
    // Round: the clip inside the inner ring, the track below past the outer.
    let radial = MaskShape::RadialGradient { center_x: 0.5, center_y: 0.5, inner: 0.1, outer: 0.3 };
    for (path, [centre, corner]) in ["CPU", "GPU"].into_iter().zip(render(Some(&faded(radial)))) {
        assert!(near(centre, clip), "{path}: inside, the clip: {centre:?} vs {clip:?}");
        assert!(near(corner, below), "{path}: outside, the track below: {corner:?} vs {below:?}");
    }
}

/// A cut-out clip shrunk by a Transform is cut where its picture lands, on
/// the GPU stack as on the CPU route: the alpha is taken where the grade
/// ends, before the clip's effects (its placement among them) are spliced
/// in, so they move picture and alpha together.
#[test]
fn a_placed_cut_out_clip_is_cut_where_it_lands_on_the_stack() {
    let shrink = klipp_effects::BuiltinEffect::Transform { position_x: 0.0, position_y: 0.0, scale_x: 0.6, scale_y: 0.6, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };
    // With an effect beside it, so the clip's effects are spliced into
    // its grade's run on the stack rather than drawn as its layer's pose.
    let top = klipp_timeline::test_clip::TestClip::of(graded(true)).effects([shrink, klipp_effects::BuiltinEffect::Invert { mix: 0.25 }]).build();
    let below = media();
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = [("V2", &top), ("V1", &below)]
        .map(|(name, clip)| {
            let mut t = Track::new(name, TrackKind::Video);
            t.clips = vec![clip.clone()];
            t
        })
        .into();
    let mut src = Grey::default();
    src.0.insert(top.id, 0.18);
    src.0.insert(below.id, 0.6);
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    // The viewer's CPU encode, dithered as the stack's is (#2072).
    let cpu = klipp_color::display::display_transform_ws_dithered_to(
        &r.render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()),
        klipp_color::display::ToneMapOp::None,
        klipp_core::WorkingColorspace::Rec709Linear,
        klipp_core::SdrTransfer::Srgb,
    );
    let Some((stack, _)) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) else {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but the stack declined: {:?}", r.last_decline());
        eprintln!("UNVERIFIED: the stack declined ({:?})", r.last_decline());
        return;
    };
    // The cut-out shows at all: the shrunk circle's centre is the clip,
    // a corner the track below.
    let at = |img: &[u8], x: u32, y: u32| img[((y * W + x) * 4) as usize];
    assert!(at(&cpu, W / 2, H / 2) < at(&cpu, 1, 1), "the clip (darker) in the middle, the track below at the corner");
    // The circle shrinks with the picture: 0.25 of the width at 0.6 is
    // under 5 px, so 6 px right of the centre is cut on both routes. Taken
    // after the placement it would stay 8 px across and show the clip.
    // Both points show the same flat grey of the track below, so they can
    // differ only by the display dither at their two positions (#2072):
    // a level at most, where the clip would be dozens darker.
    for (route, img) in [("CPU", &cpu), ("stack", &stack)] {
        let (cut, below) = (at(img, W / 2 + 6, H / 2), at(img, 1, 1));
        assert!(cut.abs_diff(below) <= 1, "{route}: cut where the shrunk circle ends: {cut} vs {below}");
    }
    let worst = cpu.iter().zip(&stack).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
    assert!(worst <= 2, "the stack cut the placed clip elsewhere than the CPU route: {worst} levels apart");
}

#[test]
fn restore_background_matches_the_frame_before_cut_out() {
    let path = std::env::temp_dir().join(format!("restore-background-{}.png", ClipId::new()));
    image::GrayImage::from_fn(W, H, |x, _| image::Luma([if x < W / 2 { 0 } else { 255 }])).save(&path).unwrap();
    let mut clip = graded(false);
    let before = clip.grade().clone();
    let mut cut = before.clone();
    let id = klipp_nodes::NodeId::new_v4();
    cut.nodes.push(klipp_nodes::Node {
        id, kind: NodeKind::ExternalMatte, label: "Cut out".into(), enabled: true,
        params: klipp_nodes::NodeParams { matte_image_path: Some(path.clone()), ..Default::default() },
        keyframes: Default::default(), output_link: None, input_links: Vec::new(),
    });
    cut.edges.push(Edge { from_node: id, from_port: 0, to_node: cut.output_node_id, to_port: 1 });
    let mut restored = cut.clone();
    assert!(restored.restore_background());
    let mut renderer = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    renderer.set_gpu_enabled(false);
    let mut source = Grey::default();
    source.0.insert(clip.id, 0.18);
    let frames = [before, cut, restored].map(|graph| {
        clip.set_grade(graph);
        let mut seq = Sequence::new_default();
        seq.resolution = klipp_core::Resolution { width: W, height: H };
        let mut track = Track::new("V1", TrackKind::Video);
        track.clips = vec![clip.clone()];
        seq.tracks = vec![track];
        renderer.render(&seq, tc(1.0), W, H, &mut source, RenderOptions::default())
    });
    std::fs::remove_file(path).unwrap();
    assert_ne!(frames[0], frames[1], "the matte must visibly cut the frame");
    assert_eq!(frames[0], frames[2], "restore returns every pixel to the original");
}
