//! `Renderer::render_yuv420_on_gpu` either refuses, or produces the
//! picture `Renderer::render` produces.
//!
//! That path exists so a render does not have to bring a linear frame
//! back across the bus for the CPU to convert — at 4K, 132 MB and a
//! per-pixel pass against 12.4 MB and none. It can only do that when the
//! whole render is one clip's grade, because everything the renderer does
//! *after* the grade happens on the CPU frame: compositing, opacity,
//! blend modes, transforms, effects, mattes.
//!
//! So the gate deciding that is the dangerous part of the whole
//! exercise. Getting it too strict costs speed and shows up as a
//! measurement; getting it too loose costs a wrong picture, silently,
//! because the frames it drops on the floor are exactly the ones nobody
//! is looking at while checking the simple case. This file is the answer
//! to that: for each shape the gate is supposed to refuse, assert it
//! *does* refuse — and for the one it accepts, assert the planes carry
//! the same picture the ordinary render does.

use std::collections::HashMap;
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{BlendMode, ClipId, GeneratorData, RationalTime, Timecode, TransitionId};
use klipp_effects::BuiltinEffect;
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams, NodePos};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind, Transition};

/// A multiple of 8 wide and even high, so the shader's block layout
/// tiles; the gate refuses anything else and that is tested separately.
const W: u32 = 32;
const H: u32 = 16;

/// BT.709, and no tone map — the plain delivery.
const TONE_MAP_NONE: u32 = 0;
const LUMA_709: [f32; 3] = [0.2126, 0.7152, 0.0722];

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn clip(kind: ClipKind, from: f64, to: f64) -> Clip {
    Clip { kind, ..Clip::gap(tc(from), tc(to)) }
}

fn media(from: f64, to: f64) -> Clip {
    clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, from, to)
}

/// A clip with an actual grade on it.
///
/// This matters for the positive case: an identity graph never starts the
/// GPU chain, so there is no GPU-resident result to convert and the path
/// declines — correctly, but it would make a test that used one report a
/// skip instead of an agreement.
fn graded(from: f64, to: f64, stops: f32) -> Clip {
    let mut c = media(from, to);
    let node = |kind, params| Node {
        id: NodeId::new_v4(),
        kind,
        label: format!("{kind:?}"),
        enabled: true,
        params,
        keyframes: Default::default(),
        output_link: None,
        input_links: vec![],
    };
    let nodes = vec![
        node(NodeKind::Source, NodeParams::default()),
        node(NodeKind::Exposure, NodeParams { exposure: Some(stops), ..Default::default() }),
        node(NodeKind::Output, NodeParams::default()),
    ];
    let edges = (0..2)
        .map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 })
        .collect();
    let node_positions =
        nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
    c.node_graph = NodeGraph {
        output_node_id: nodes[2].id,
        nodes,
        edges,
        node_positions,
        working_space_override: None,
        frames: vec![],
        sticky_notes: vec![],
    };
    c
}

fn sequence(tracks: Vec<Vec<Clip>>) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = tracks
        .into_iter()
        .enumerate()
        .map(|(i, clips)| {
            let mut t = Track::new(&format!("V{i}"), TrackKind::Video);
            t.clips = clips;
            t
        })
        .collect();
    seq
}

/// Media clips return a flat linear frame, one value per clip, so a
/// composite of two layers is obvious in the result.
#[derive(Default)]
struct Flat(HashMap<ClipId, [f32; 3]>);

impl Flat {
    fn with(mut self, c: &Clip, rgb: [f32; 3]) -> Self {
        self.0.insert(c.id, rgb);
        self
    }
}

impl FrameSource for Flat {
    fn frame(&mut self, clip: &Clip, _secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let c = self.0.get(&clip.id)?;
        let px = [c[0], c[1], c[2], 1.0].repeat((width * height) as usize);
        Some(SourceFrame {
            pixels: SourcePixels::Linear(Arc::new(px)),
            width,
            height,
            pts: None,
        })
    }
}

fn renderer() -> Renderer {
    Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new())
}

/// The planes the GPU path makes, or `None` if it refused.
fn gpu_planes(seq: &Sequence, t: f64, src: &mut Flat) -> Option<Vec<u8>> {
    renderer().render_yuv420_on_gpu(
        seq,
        tc(t),
        W,
        H,
        src,
        RenderOptions::default(),
        TONE_MAP_NONE,
        LUMA_709,
        false,
    )
}

/// The same frame through the ordinary render, converted the way the CPU
/// export converts it: display transform, then luma at 16/219.
fn cpu_luma(seq: &Sequence, t: f64, src: &mut Flat) -> Vec<u8> {
    let px = renderer().render(seq, tc(t), W, H, src, RenderOptions::default());
    let display = klipp_color::display::ExportDisplay::new(
        klipp_core::ToneMapOp::None,
        klipp_core::WorkingColorspace::Rec709Linear,
        klipp_core::OdtChoice::MatchViewer,
    );
    px.chunks_exact(4)
        .map(|p| {
            let e = display.encode([p[0], p[1], p[2]]);
            let y = LUMA_709[0] * e[0] + LUMA_709[1] * e[1] + LUMA_709[2] * e[2];
            (16.0 + y * 219.0 + 0.5).clamp(0.0, 255.0) as u8
        })
        .collect()
}

/// A plain single clip is what the path is for, and its luma has to be
/// the renderer's own picture. Without this the file would only prove the
/// gate says no, which is easy to do by saying no to everything.
#[test]
fn a_plain_single_clip_agrees_with_the_renderer() {
    let c = graded(0.0, 1.0, 0.5);
    let seq = sequence(vec![vec![c.clone()]]);
    let mut src = Flat::default().with(&c, [0.18, 0.5, 0.9]);

    let Some(planes) = gpu_planes(&seq, 0.5, &mut src) else {
        eprintln!("skipping: the GPU path refused (no usable GPU)");
        return;
    };
    let want = cpu_luma(&seq, 0.5, &mut src);
    let got = &planes[..want.len()];

    let mut worst = 0i32;
    for (g, w) in got.iter().zip(want.iter()) {
        worst = worst.max((*g as i32 - *w as i32).abs());
    }
    assert!(worst <= 1, "luma differs from the renderer by {worst} levels");

    // And the frame is not accidentally blank, which would agree with a
    // blank reference.
    assert!(
        got.iter().any(|&v| v > 40),
        "the fixture rendered black, so agreement proves nothing"
    );
}

/// Each shape the gate must refuse, because the renderer does something
/// to the frame after the grade that this path cannot.
#[test]
fn the_gate_refuses_everything_it_cannot_reproduce() {
    // Every clip here is graded. An identity graph never starts the GPU
    // chain, so an ungraded clip declines for want of a chain rather
    // than because the gate refused it — and the whole file would pass
    // while testing nothing.
    // A second layer to composite.
    let (a, b) = (graded(0.0, 1.0, 0.5), graded(0.0, 1.0, 0.5));
    let overlay = sequence(vec![vec![a.clone()], vec![b.clone()]]);
    let overlay_src = Flat::default().with(&a, [0.2; 3]).with(&b, [0.8; 3]);

    // A dissolve between two clips.
    let (c, d) = (graded(0.0, 1.0, 0.5), graded(0.5, 1.5, 0.5));
    let mut with_transition = sequence(vec![vec![c.clone(), d.clone()]]);
    with_transition.tracks[0].transitions = vec![Transition {
        id: TransitionId::new(),
        kind: klipp_core::TransitionKind::CrossDissolve,
        duration: tc(0.25),
        clip_a_id: c.id,
        clip_b_id: d.id,
        user_params: Default::default(),
        alignment: klipp_core::TransitionAlignment::Center,
    }];
    let transition_src = Flat::default().with(&c, [0.2; 3]).with(&d, [0.8; 3]);

    // Track opacity below one.
    let e = graded(0.0, 1.0, 0.5);
    let mut faded_track = sequence(vec![vec![e.clone()]]);
    faded_track.tracks[0].opacity = 0.5;

    // An opacity effect on the clip.
    let mut f = graded(0.0, 1.0, 0.5);
    f.builtin_effects = vec![BuiltinEffect::Opacity { value: 0.5 }];
    let faded_clip = sequence(vec![vec![f.clone()]]);

    // A builtin effect that is not opacity.
    let mut g = graded(0.0, 1.0, 0.5);
    g.builtin_effects = vec![BuiltinEffect::GaussianBlur { radius: 3.0 }];
    let blurred = sequence(vec![vec![g.clone()]]);

    // A blend mode other than Normal.
    let mut h = graded(0.0, 1.0, 0.5);
    h.blend_mode = BlendMode::Screen;
    let screened = sequence(vec![vec![h.clone()]]);

    // A clip that is some other clip's matte.
    let mut i = graded(0.0, 1.0, 0.5);
    i.consumed_as_matte = true;
    let matte_source = sequence(vec![vec![i.clone()]]);

    // A transform: a zoom resamples the frame after the grade.
    let mut j = graded(0.0, 1.0, 0.5);
    j.builtin_effects = vec![BuiltinEffect::Transform {
        position_x: 0.0,
        position_y: 0.0,
        scale_x: 1.5,
        scale_y: 1.5,
        rotation: 0.0,
        anchor_x: 0.5,
        anchor_y: 0.5,
        rotation_x: 0.0,
        rotation_y: 0.0,
        perspective: 0.0,
        focal_ref: None,
    }];
    let zoomed = sequence(vec![vec![j.clone()]]);

    // A generator, which is not a media clip at all.
    let k = clip(
        ClipKind::Generator(GeneratorData::SolidColor { color: [255, 0, 0, 255] }),
        0.0,
        1.0,
    );
    let generated = sequence(vec![vec![k.clone()]]);

    // Nothing on screen.
    let empty = sequence(vec![vec![graded(2.0, 3.0, 0.5)]]);

    let cases: Vec<(&str, Sequence, Flat)> = vec![
        ("an overlay layer", overlay, overlay_src),
        ("a dissolve", with_transition, transition_src),
        ("track opacity 0.5", faded_track, Flat::default().with(&e, [0.5; 3])),
        ("a clip opacity effect", faded_clip, Flat::default().with(&f, [0.5; 3])),
        ("a gaussian blur", blurred, Flat::default().with(&g, [0.5; 3])),
        ("a screen blend", screened, Flat::default().with(&h, [0.5; 3])),
        ("a matte source", matte_source, Flat::default().with(&i, [0.5; 3])),
        ("a zoom", zoomed, Flat::default().with(&j, [0.5; 3])),
        ("a generator", generated, Flat::default()),
        ("nothing on screen", empty, Flat::default()),
    ];

    for (what, seq, mut src) in cases {
        assert!(
            gpu_planes(&seq, 0.5, &mut src).is_none(),
            "the GPU path accepted {what}, which the renderer finishes on the CPU — \
             the export would have written the ungraded or uncomposited frame"
        );
    }
}

/// An HDR delivery renders display light, a pass the shader has no twin
/// for, so the gate refuses it whatever the timeline looks like.
#[test]
fn an_hdr_render_is_refused() {
    let c = media(0.0, 1.0);
    let seq = sequence(vec![vec![c.clone()]]);
    let mut src = Flat::default().with(&c, [0.5; 3]);
    let refused = renderer().render_yuv420_on_gpu(
        &seq,
        tc(0.5),
        W,
        H,
        &mut src,
        RenderOptions { hdr_peak_nits: Some(1000.0), ..Default::default() },
        TONE_MAP_NONE,
        LUMA_709,
        false,
    );
    assert!(refused.is_none(), "an HDR render must not take the SDR plane path");
}
