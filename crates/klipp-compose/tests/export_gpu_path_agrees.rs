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

/// BT.709, no tone map, 8-bit planar — the plain delivery.
const LUMA_709: [f32; 3] = [0.2126, 0.7152, 0.0722];

fn plain_target() -> klipp_effects::gpu::Yuv420Target {
    klipp_effects::gpu::Yuv420Target {
        tone_map_op: 0,
        odt_tag: 0,
        gamut_op: 0,
        hdr_op: 0,
        peak_nits: 0.0,
        luma_coefs: LUMA_709,
        semi_planar: false,
        bits: 8,
        sample_shift: 0,
    }
}

/// The same delivery out of a BT.2020 project: the basis change and the
/// BT.2407 compression into BT.709 run on the GPU ahead of the encode.
fn bt2020_target() -> klipp_effects::gpu::Yuv420Target {
    klipp_effects::gpu::Yuv420Target { gamut_op: 1, ..plain_target() }
}

/// A PQ delivery: ten bits, P010, BT.2020 luma, mastered to `peak`.
fn pq_target(peak: f32) -> klipp_effects::gpu::Yuv420Target {
    klipp_effects::gpu::Yuv420Target {
        hdr_op: 1,
        peak_nits: peak,
        luma_coefs: [0.2627, 0.6780, 0.0593],
        semi_planar: true,
        bits: 10,
        sample_shift: 6,
        ..plain_target()
    }
}

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
/// This matters for the refusal cases. An identity graph takes a
/// different route through `render_yuv420_on_gpu` — the pass-through,
/// which seeds the chain and converts that — so a refusal case built on
/// an ungraded clip would be testing the wrong branch, and before the
/// pass-through existed it declined for a reason that had nothing to do
/// with the gate.
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

/// Media clips return a flat frame, one colour per clip, so a composite
/// of two layers is obvious in the result.
///
/// `on_gpu` decides which shape it arrives in, and that matters: the
/// export's frames are always `SourcePixels::Gpu` now — a hardware
/// decode seeds from NV12 and a software one from the scaler's samples —
/// so a CPU-only fixture cannot reach the identity pass-through at all.
#[derive(Default)]
struct Flat {
    colours: HashMap<ClipId, [u8; 3]>,
    on_gpu: bool,
}

impl Flat {
    /// The colour is given as an sRGB byte triple rather than a linear
    /// one so it survives the GPU seed exactly: the seed uploads sRGB
    /// bytes, and a linear value that is not the image of a byte would
    /// come back a rounding away from the reference.
    fn with(mut self, c: &Clip, srgb: [u8; 3]) -> Self {
        self.colours.insert(c.id, srgb);
        self
    }

    fn on_gpu(mut self) -> Self {
        self.on_gpu = true;
        self
    }

    fn linear(srgb: [u8; 3], n: usize) -> Vec<f32> {
        let c = |v: u8| klipp_color::transfer::srgb_to_linear(v as f32 / 255.0);
        [c(srgb[0]), c(srgb[1]), c(srgb[2]), 1.0].repeat(n)
    }
}

/// A frame the decoder left on the GPU, seeded from its sRGB bytes the
/// way the viewer's hardware-decode path does.
struct SeededFlat {
    srgb: Arc<Vec<u8>>,
    linear: Arc<Vec<f32>>,
}

impl klipp_compose::GpuPixels for SeededFlat {
    fn seed(&self, exec: &mut klipp_nodes::GraphExecutor, width: u32, height: u32) -> bool {
        exec.prime_chain_from_srgb8(&self.srgb, width, height)
    }

    fn read(&self) -> Arc<Vec<f32>> {
        Arc::clone(&self.linear)
    }
}

impl FrameSource for Flat {
    fn frame(&mut self, clip: &Clip, _secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let srgb = *self.colours.get(&clip.id)?;
        let n = (width * height) as usize;
        let linear = Arc::new(Flat::linear(srgb, n));
        let pixels = if self.on_gpu {
            SourcePixels::Gpu(Arc::new(SeededFlat {
                srgb: Arc::new([srgb[0], srgb[1], srgb[2], 255].repeat(n)),
                linear,
            }))
        } else {
            SourcePixels::Linear(linear)
        };
        Some(SourceFrame { pixels, width, height, pts: None })
    }
}

fn renderer() -> Renderer {
    Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new())
}

/// The planes the GPU path makes, or `None` if it refused.
fn gpu_planes(seq: &Sequence, t: f64, src: &mut Flat) -> Option<Vec<u8>> {
    renderer()
        .render_yuv420_on_gpu(seq, tc(t), W, H, src, RenderOptions::default(), plain_target())
        .map(|(bytes, _)| bytes)
}

/// [`gpu_planes`] in a given working space, with the matching target.
fn gpu_planes_ws(
    seq: &Sequence,
    t: f64,
    src: &mut Flat,
    ws: klipp_core::WorkingColorspace,
    target: klipp_effects::gpu::Yuv420Target,
) -> Option<Vec<u8>> {
    let mut r = renderer();
    r.set_working_colorspace(ws);
    r.render_yuv420_on_gpu(seq, tc(t), W, H, src, RenderOptions::default(), target)
        .map(|(bytes, _)| bytes)
}

/// The same frame through the ordinary render, converted the way the CPU
/// export converts it: display transform, then luma at 16/219.
fn cpu_luma(seq: &Sequence, t: f64, src: &mut Flat) -> Vec<u8> {
    cpu_luma_ws(seq, t, src, klipp_core::WorkingColorspace::Rec709Linear)
}

/// [`cpu_luma`] for a project in `ws`, whose display transform is then
/// more than a gamma — `ExportDisplay` knows which.
fn cpu_luma_ws(
    seq: &Sequence,
    t: f64,
    src: &mut Flat,
    ws: klipp_core::WorkingColorspace,
) -> Vec<u8> {
    let mut r = renderer();
    r.set_working_colorspace(ws);
    let px = r.render(seq, tc(t), W, H, src, RenderOptions::default());
    let display = klipp_color::display::ExportDisplay::new(
        klipp_core::ToneMapOp::None,
        ws,
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
///
/// Run from both source shapes. The export's frames arrive on the GPU,
/// but a grade can also start from CPU pixels and end up resident, and
/// the two take different arms of `grade`.
#[test]
fn a_plain_single_clip_agrees_with_the_renderer() {
    for on_gpu in [false, true] {
        let c = graded(0.0, 1.0, 0.5);
        let seq = sequence(vec![vec![c.clone()]]);
        let mut src = Flat::default().with(&c, [46, 128, 230]);
        if on_gpu {
            src = src.on_gpu();
        }

        let Some(planes) = gpu_planes(&seq, 0.5, &mut src) else {
            eprintln!("skipping: the GPU path refused (no usable GPU)");
            return;
        };
        let want = cpu_luma(&seq, 0.5, &mut src);
        let got = &planes[..want.len()];

        let worst = got
            .iter()
            .zip(want.iter())
            .map(|(g, w)| (*g as i32 - *w as i32).abs())
            .max()
            .unwrap_or(0);
        assert!(worst <= 1, "on_gpu={on_gpu}: luma differs from the renderer by {worst} levels");

        // And the frame is not accidentally blank, which would agree
        // with a blank reference.
        assert!(
            got.iter().any(|&v| v > 40),
            "on_gpu={on_gpu}: the fixture rendered black, so agreement proves nothing"
        );
    }
}

/// An *ungraded* clip takes the path as well, and gives the renderer's
/// picture.
///
/// It is a separate case in the code and so a separate case here:
/// `grade` short-circuits an identity graph by returning the source
/// pixels, which for a GPU frame means downloading the 132 MB this path
/// exists not to download. Before the pass-through it declined, and an
/// ungraded timeline exported at the old speed.
#[test]
fn an_ungraded_clip_agrees_with_the_renderer() {
    let c = media(0.0, 1.0);
    assert!(c.node_graph.is_identity(), "the fixture must be ungraded");
    let seq = sequence(vec![vec![c.clone()]]);
    let mut src = Flat::default().with(&c, [46, 128, 230]).on_gpu();

    let Some(planes) = gpu_planes(&seq, 0.5, &mut src) else {
        eprintln!("skipping: the GPU path refused (no usable GPU)");
        return;
    };
    let want = cpu_luma(&seq, 0.5, &mut src);
    let got = &planes[..want.len()];
    let worst = got
        .iter()
        .zip(want.iter())
        .map(|(g, w)| (*g as i32 - *w as i32).abs())
        .max()
        .unwrap_or(0);
    assert!(worst <= 1, "ungraded luma differs from the renderer by {worst} levels");
    assert!(
        got.iter().any(|&v| v > 40),
        "the fixture rendered black, so agreement proves nothing"
    );
}

/// A BT.2020 project's delivery, whose display transform is a basis
/// change and a gamut compression before the encode — all of it on the
/// GPU now, ahead of the Y'CbCr conversion in the same dispatch.
///
/// The second half is what makes the first half mean something. The
/// generic encode, which is what this path did before the shader had the
/// gamut tables, is also a plausible-looking answer: it produces a
/// picture, just an over-saturated one. Measuring how far that answer is
/// from the right one on *this* fixture proves the luma comparison can
/// tell them apart.
#[test]
fn a_bt2020_project_agrees_with_the_renderer() {
    use klipp_core::WorkingColorspace::{Bt2020Linear, Rec709Linear};
    let c = graded(0.0, 1.0, 0.5);
    let seq = sequence(vec![vec![c.clone()]]);
    // The fixture is one flat colour, so it samples the difference
    // between the two transforms exactly once — it has to be a colour
    // where they diverge. This cyan is the widest of a coarse sweep of
    // the cube: 110 code values apart in RGB, 20 in luma, because
    // BT.2020's basis puts it far outside BT.709 with room left in the
    // gamut to move it rather than simply clipping.
    let mut src = Flat::default().with(&c, [144, 224, 240]).on_gpu();

    let Some(planes) = gpu_planes_ws(&seq, 0.5, &mut src, Bt2020Linear, bt2020_target()) else {
        eprintln!("skipping: the GPU path refused (no usable GPU)");
        return;
    };
    let want = cpu_luma_ws(&seq, 0.5, &mut src, Bt2020Linear);
    let got = &planes[..want.len()];
    let worst = |a: &[u8], b: &[u8]| {
        a.iter().zip(b.iter()).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0)
    };
    let w = worst(got, &want);
    eprintln!("BT.2020 export luma vs the renderer: worst {w} of 255");
    assert!(w <= 1, "BT.2020 luma differs from the renderer by {w} levels");
    assert!(got.iter().any(|&v| v > 40), "the fixture rendered black, so agreement proves nothing");

    // The same shader with the gamut branch off — the mistake this
    // replaces, and a plausible-looking picture in its own right. Whole
    // planes, not just luma: a gamut map moves chroma far more than it
    // moves brightness, so Cb and Cr are where the difference lives and
    // where a comparison has room to be sure.
    let ungamuted = gpu_planes_ws(&seq, 0.5, &mut src, Bt2020Linear, plain_target())
        .expect("the path takes the same frame without the gamut branch");
    let d = worst(&planes, &ungamuted);
    eprintln!("the same shader without the gamut branch differs by {d} of 255");
    assert!(d > 20, "only {d}: this fixture cannot tell the two encodes apart");


    // And the CPU agrees about which of the two it wanted, so the
    // difference above is the gamut map and not some unrelated drift.
    let as_if_709 = cpu_luma_ws(&seq, 0.5, &mut src, Rec709Linear);
    assert!(
        worst(&want, &as_if_709) > worst(got, &want),
        "the CPU's two transforms differ by less than the GPU matches by"
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
    let overlay_src = Flat::default().with(&a, [80, 80, 80]).with(&b, [200, 200, 200]);

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
    let transition_src = Flat::default().with(&c, [80, 80, 80]).with(&d, [200, 200, 200]);

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
        ("track opacity 0.5", faded_track, Flat::default().with(&e, [128, 128, 128])),
        ("a clip opacity effect", faded_clip, Flat::default().with(&f, [128, 128, 128])),
        ("a gaussian blur", blurred, Flat::default().with(&g, [128, 128, 128])),
        ("a screen blend", screened, Flat::default().with(&h, [128, 128, 128])),
        ("a matte source", matte_source, Flat::default().with(&i, [128, 128, 128])),
        ("a zoom", zoomed, Flat::default().with(&j, [128, 128, 128])),
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

/// The render and the encode must be for the same delivery.
///
/// The target says whether the *encoder* wants HDR and `RenderOptions`
/// says whether the *render* is producing display light for one. Both
/// come from one `ExportSettings`, so a mismatch is a wiring mistake —
/// and it would show as a wrong picture, not an error, which is why the
/// gate checks rather than assumes.
#[test]
fn a_mismatched_delivery_is_refused() {
    let c = media(0.0, 1.0);
    let seq = sequence(vec![vec![c.clone()]]);
    let hdr = RenderOptions { hdr_peak_nits: Some(1000.0), ..Default::default() };
    for (what, opts, target) in [
        ("an HDR render into an SDR encode", hdr, plain_target()),
        ("an SDR render into a PQ encode", RenderOptions::default(), pq_target(1000.0)),
    ] {
        let mut src = Flat::default().with(&c, [128, 128, 128]);
        let refused =
            renderer().render_yuv420_on_gpu(&seq, tc(0.5), W, H, &mut src, opts, target);
        assert!(refused.is_none(), "the GPU path accepted {what}");
    }
}

/// A PQ delivery through the whole path, against the renderer.
///
/// This is the case that used to be refused twice over: the display-light
/// pass had no shader twin, and MaxCLL is measured per pixel in cd/m²
/// off the linear frame. The first refusal still stands for a
/// *scene-referred* source, which ends in its own display pass — but an
/// ordinary clip in an HDR delivery has no such pass, and its working
/// values simply *are* display light, which is what the shader now
/// encodes.
#[test]
fn a_pq_delivery_agrees_with_the_renderer() {
    let c = graded(0.0, 1.0, 0.5);
    let seq = sequence(vec![vec![c.clone()]]);
    let mut src = Flat::default().with(&c, [230, 180, 90]).on_gpu();
    let opts = RenderOptions { hdr_peak_nits: Some(1000.0), ..Default::default() };

    let Some((planes, light)) =
        renderer().render_yuv420_on_gpu(&seq, tc(0.5), W, H, &mut src, opts, pq_target(1000.0))
    else {
        eprintln!("skipping: the GPU path refused (no usable GPU)");
        return;
    };

    // The renderer's own frame, encoded the way the CPU export encodes a
    // PQ delivery, and its light accumulated the same way.
    let mut r = renderer();
    let px = r.render(&seq, tc(0.5), W, H, &mut src, opts);
    let display = klipp_color::display::ExportDisplay::hdr(
        klipp_core::WorkingColorspace::Rec709Linear,
        klipp_core::HdrMode::Pq,
        1000.0,
    );
    let k = [0.2627f32, 0.6780, 0.0593];
    let mut want = Vec::with_capacity((W * H) as usize);
    let (mut max, mut sum) = (0.0f32, 0.0f64);
    for p in px.chunks_exact(4) {
        let (e, nits) = display.encode_light([p[0], p[1], p[2]]);
        max = max.max(nits);
        sum += nits as f64;
        let y = k[0] * e[0] + k[1] * e[1] + k[2] * e[2];
        want.push(((16.0 + y * 219.0) * 4.0 + 0.5).clamp(0.0, 1023.0) as u16);
    }

    // Ten bits, P010, so the luma plane is the first `W*H` u16 with its
    // ten bits at the top of each.
    let got: Vec<u16> = planes[..want.len() * 2]
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]) >> 6)
        .collect();
    let worst =
        got.iter().zip(want.iter()).map(|(g, w)| (*g as i32 - *w as i32).abs()).max().unwrap_or(0);
    eprintln!("PQ export luma vs the renderer: worst {worst} of 1023");
    assert!(worst <= 1, "PQ luma differs from the renderer by {worst} levels");
    assert!(got.iter().any(|&v| v > 200), "the fixture rendered black");

    // And the light the file's MaxCLL and MaxFALL are made of.
    let l = light.expect("a PQ delivery must come back with its light levels");
    let pixels = (W as u64) * (H as u64);
    assert_eq!(l.pixels, pixels);
    eprintln!("PQ light: MaxCLL gpu {:.3} cpu {max:.3} nits", l.max_nits);
    assert!((l.max_nits - max).abs() <= max * 1e-5, "MaxCLL is {} vs {max}", l.max_nits);
    let (gm, cm) = (l.sum_nits / pixels as f64, sum / pixels as f64);
    assert!((gm - cm).abs() < 0.01 + cm.abs() * 1e-5, "MaxFALL is {gm} vs {cm}");
    assert!(max > 100.0, "the fixture is too dim to measure light with");
}

/// A project-wide LUT is refused, not ignored.
///
/// The global LUT is applied after the composite, on the CPU frame this
/// path never builds — so an accepted frame would be exported *without*
/// the timeline's LUT, which is a wrong picture with no error anywhere.
/// The viewer's fast path has always refused it ("global colour pipeline
/// active"); this path did not, and exported the ungraded look.
#[test]
fn a_global_lut_is_refused() {
    let c = graded(0.0, 1.0, 0.5);
    let seq = sequence(vec![vec![c.clone()]]);
    let mut src = Flat::default().with(&c, [128, 96, 64]).on_gpu();

    // Not an identity LUT: the refusal must not depend on the LUT being
    // a no-op, since a real one is the case that matters.
    let size = 2usize;
    let mut data = Vec::with_capacity(size * size * size * 3);
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                let f = |v: usize| (v as f32 / (size - 1) as f32) * 0.5;
                data.extend_from_slice(&[f(r), f(g), f(b)]);
            }
        }
    }
    let lut = klipp_color::lut::LutData {
        size,
        data,
        input_space: klipp_core::WorkingColorspace::Rec709Linear,
        output_space: klipp_core::WorkingColorspace::Rec709Linear,
    };

    let mut r = renderer();
    r.set_global_lut(Some(klipp_compose::GlobalLut { lut, opacity: 1.0 }));
    let refused = r.render_yuv420_on_gpu(
        &seq,
        tc(0.5),
        W,
        H,
        &mut src,
        RenderOptions::default(),
        plain_target(),
    );
    assert!(
        refused.is_none(),
        "the GPU path accepted a timeline with a global LUT, so the export would have \
         written the picture without it"
    );

    // And without the LUT the same frame *is* accepted, so the refusal
    // above is the LUT and not the fixture.
    let mut r = renderer();
    assert!(
        r.render_yuv420_on_gpu(
            &seq,
            tc(0.5),
            W,
            H,
            &mut src,
            RenderOptions::default(),
            plain_target(),
        )
        .is_some(),
        "the fixture cannot take the GPU path at all, so this proves nothing"
    );
}

/// The AcesCG-basis ODTs through the whole path, against the renderer.
///
/// These were the last deliveries `gpu_yuv420_params` refused. They are
/// four configurations of two chains rather than four transforms, which
/// is exactly why the end-to-end check matters: a blob built for the
/// wrong deliverable still renders a plausible picture, just the wrong
/// one. So each is compared against its own `ExportDisplay`, and then
/// against the others to show the comparison can tell them apart.
///
/// Run from a BT.709 project as well as an AcesCG one, because an ODT is
/// defined on AP1 and a project in any other basis has to be walked
/// there first.
#[test]
fn the_acescg_odts_agree_with_the_renderer() {
    use klipp_core::{OdtChoice, WorkingColorspace as W};
    // Two exposures, because the deliverables do not all differ at one.
    // The two PQ peaks are identical in mid-tones on purpose — PQ encodes
    // absolute luminance, so only highlight roll-off moves with the peak
    // (`pq_mid_grey_is_invariant_to_peak_luminance` in klipp-color pins
    // that) — so telling those two apart needs a real highlight, while
    // the SDR pair would be clipped to white by it.
    let mut seen_apart: Vec<(OdtChoice, OdtChoice)> = Vec::new();
    for stops in [0.5f32, 5.0] {
    let c = graded(0.0, 1.0, stops);
    let seq = sequence(vec![vec![c.clone()]]);

    for working in [W::AcesCG, W::Rec709Linear] {
        let mut pictures: Vec<(OdtChoice, Vec<u8>)> = Vec::new();
        for odt in [
            OdtChoice::Rec709Sdr,
            OdtChoice::DciP3D65_48,
            OdtChoice::Rec2100Pq1000,
            OdtChoice::Rec2100Pq4000,
        ] {
            let display = klipp_color::display::ExportDisplay::new(
                klipp_core::ToneMapOp::None,
                working,
                odt,
            );
            let tag = display
                .acescg_basis_odt()
                .expect("every one of these is an AcesCG-basis ODT")
                .tag();
            let target = klipp_effects::gpu::Yuv420Target {
                tone_map_op: 3,
                odt_tag: tag,
                ..plain_target()
            };
            let mut src = Flat::default().with(&c, [210, 120, 60]).on_gpu();
            let mut rg = renderer();
            rg.set_working_colorspace(working);
            let Some((planes, light)) = rg.render_yuv420_on_gpu(
                &seq,
                tc(0.5),
                W,
                H,
                &mut src,
                RenderOptions::default(),
                target,
            ) else {
                eprintln!("skipping: the GPU path refused (no usable GPU)");
                return;
            };
            assert!(light.is_none(), "{odt:?}: an ODT delivery measures no light");

            // The renderer's frame through the same ExportDisplay.
            let mut r = renderer();
            r.set_working_colorspace(working);
            let px = r.render(&seq, tc(0.5), W, H, &mut src, RenderOptions::default());
            let want: Vec<u8> = px
                .chunks_exact(4)
                .map(|p| {
                    let e = display.encode([p[0], p[1], p[2]]);
                    let y = LUMA_709[0] * e[0] + LUMA_709[1] * e[1] + LUMA_709[2] * e[2];
                    (16.0 + y * 219.0 + 0.5).clamp(0.0, 255.0) as u8
                })
                .collect();
            let got = &planes[..want.len()];
            let worst = got
                .iter()
                .zip(want.iter())
                .map(|(g, w)| (*g as i32 - *w as i32).abs())
                .max()
                .unwrap_or(0);
            eprintln!("{odt:?} from {working:?} at {stops:+} stops: worst {worst} of 255");
            assert!(
                worst <= 1,
                "{odt:?} from {working:?} at {stops:+} stops: differs by {worst} levels",
            );
            assert!(got.iter().any(|&v| v > 40), "{odt:?}: the fixture rendered black");
            pictures.push((odt, planes));
        }

        // Note which pairs this exposure separates.
        for i in 0..pictures.len() {
            for j in (i + 1)..pictures.len() {
                let (a, b) = (&pictures[i], &pictures[j]);
                let d = a
                    .1
                    .iter()
                    .zip(b.1.iter())
                    .map(|(x, y)| (*x as i32 - *y as i32).abs())
                    .max()
                    .unwrap_or(0);
                if d > 4 {
                    seen_apart.push((a.0, b.0));
                }
            }
        }
    }
    }

    // Every pair has to be separated by *some* exposure. Without this the
    // agreement above would also hold for a shader that ignored the
    // blob's variant and ran one chain for all four.
    for (i, a) in [
        OdtChoice::Rec709Sdr,
        OdtChoice::DciP3D65_48,
        OdtChoice::Rec2100Pq1000,
        OdtChoice::Rec2100Pq4000,
    ]
    .iter()
    .enumerate()
    {
        for b in [
            OdtChoice::Rec709Sdr,
            OdtChoice::DciP3D65_48,
            OdtChoice::Rec2100Pq1000,
            OdtChoice::Rec2100Pq4000,
        ]
        .iter()
        .skip(i + 1)
        {
            assert!(
                seen_apart.contains(&(*a, *b)),
                "{a:?} and {b:?} rendered the same picture at every exposure tried, so \
                 this test cannot tell those two deliverables apart",
            );
        }
    }
}
