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

#[path = "support/cpu_reference.rs"]
mod cpu_reference;
use cpu_reference::{no_gpu, CpuReference};
use klipp_timeline::GradeAccess;
use std::collections::HashMap;
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{BlendMode, ClipId, GeneratorData, RationalTime, Timecode, TransitionId};
use klipp_effects::BuiltinEffect;
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams, NodePos};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind, Transition};
use klipp_timeline::test_clip::TestClip;

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
        scene: None,
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
    c.set_grade(NodeGraph {
        output_node_id: nodes[2].id,
        nodes,
        edges,
        node_positions,
        working_space_override: None,
        frames: vec![],
        sticky_notes: vec![],
    });
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
/// export's frames are mostly `SourcePixels::Gpu` — a hardware decode
/// seeds from NV12 and a software one from the scaler's samples — but a
/// reversed clip's are read backwards into memory, `SourcePixels::Linear`.
#[derive(Default)]
struct Flat {
    colours: HashMap<ClipId, [u8; 3]>,
    scene: HashMap<ClipId, klipp_color::hdr::HdrTransfer>,
    /// A scene-referred clip's primaries into the working space; the
    /// identity when absent.
    scene_matrix: HashMap<ClipId, [[f32; 3]; 3]>,
    on_gpu: bool,
}

/// The identity basis change: a source in the working space's primaries.
const SAME_PRIMARIES: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// BT.2020 primaries into BT.709: a source shot wide, in a Rec.709 project.
const BT2020_TO_709: [[f32; 3]; 3] = [[1.6605, -0.5876, -0.0728], [-0.1246, 1.1329, -0.0083], [-0.0182, -0.1006, 1.1187]];

impl Flat {
    /// The colour is given as an sRGB byte triple rather than a linear
    /// one so it survives the GPU seed exactly: the seed uploads sRGB
    /// bytes, and a linear value that is not the image of a byte would
    /// come back a rounding away from the reference.
    fn with(mut self, c: &Clip, srgb: [u8; 3]) -> Self {
        self.colours.insert(c.id, srgb);
        self
    }

    /// A scene-referred frame instead: packed 10-bit codes, as a PQ, HLG
    /// or camera-log decoder hands them over. Exact by construction —
    /// codes are what both paths start from.
    ///
    /// The codes *vary across the frame* rather than being flat, unlike
    /// the sRGB fixture above. The display-light derivation is per-pixel
    /// arithmetic with three branches and a highlight roll-off, so a flat
    /// frame would sample each of them exactly once and agree with almost
    /// anything.
    fn scene(mut self, c: &Clip, transfer: klipp_color::hdr::HdrTransfer) -> Self {
        self.scene.insert(c.id, transfer);
        self
    }

    /// [`Self::scene`] in other primaries than the working space's, so the
    /// basis change is something a route could drop or get wrong.
    fn scene_in(mut self, c: &Clip, transfer: klipp_color::hdr::HdrTransfer, to_working: [[f32; 3]; 3]) -> Self {
        self.scene_matrix.insert(c.id, to_working);
        self.scene(c, transfer)
    }

    fn on_gpu(mut self) -> Self {
        self.on_gpu = true;
        self
    }

    /// The same fixture with its frames arriving on the GPU, as most of
    /// the export's do.
    fn clone_for_gpu(&self) -> Self {
        Flat { colours: self.colours.clone(), scene: self.scene.clone(), scene_matrix: self.scene_matrix.clone(), on_gpu: true }
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
        if let Some(transfer) = self.scene.get(&clip.id).copied() {
            // A ramp down the frame with a hue rotation across it, so the
            // codes span black to peak and no two channels agree — which
            // the luma-weighted HLG branch needs to be distinguishable
            // from the per-channel ones.
            let codes: Vec<u32> = (0..height)
                .flat_map(|y| {
                    (0..width).map(move |x| {
                        let v = y * 1023 / height.max(1);
                        let t = x * 1023 / width.max(1);
                        klipp_color::hdr::pack_rgb10(
                            v.min(1023),
                            (v * 3 / 4 + t / 4).min(1023),
                            (v / 2 + t / 2).min(1023),
                        )
                    })
                })
                .collect();
            return Some(SourceFrame {
                pixels: SourcePixels::SceneReferred {
                    codes: Arc::new(klipp_color::hdr::SceneCodes::Packed10(codes)),
                    transfer,
                    source_to_working: self.scene_matrix.get(&clip.id).copied().unwrap_or(SAME_PRIMARIES),
                },
                width,
                height,
                pts: None,
            });
        }
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

/// Whether this machine's GPU takes the path at all: a plain graded clip.
fn gpu_path_available() -> bool {
    let c = graded(0.0, 1.0, 0.5);
    gpu_planes(&sequence(vec![vec![c.clone()]]), 0.5, &mut Flat::default().with(&c, [46, 128, 230]).on_gpu()).is_some()
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
    let px = r.cpu_render(seq, tc(t), W, H, src, RenderOptions::default());
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
            no_gpu("the GPU path refused (no usable GPU)");
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

/// A dark living gradient with its grain (#2073) exports, on the GPU, the
/// frame the renderer draws, grain included, so the file and the viewer
/// (held to the same render in `living_gradient_gpu`) agree. The control:
/// the grain is in the export's frame, a picture other than the one
/// without it.
#[test]
fn a_grained_gradient_exports_its_grain_as_the_renderer_draws_it() {
    let make = |grain: f32| {
        let c = clip(ClipKind::Generator(GeneratorData::LivingGradient { a: [12, 23, 32, 255], b: [29, 60, 65, 255], angle_deg: 25.0, drift_deg_per_sec: 1.5, glow: 0.18, grain }), 0.0, 4.0);
        sequence(vec![vec![c]])
    };
    let mut planes_at = Vec::new();
    for grain in [0.0, klipp_core::DEFAULT_GENERATOR_GRAIN] {
        let seq = make(grain);
        let mut src = Flat::default();
        let Some(planes) = gpu_planes(&seq, 1.2, &mut src) else {
            no_gpu("the GPU path refused a lone generator");
            return;
        };
        let want = cpu_luma(&seq, 1.2, &mut src);
        let got = &planes[..want.len()];
        let worst = got.iter().zip(&want).map(|(g, w)| (*g as i32 - *w as i32).abs()).max().unwrap_or(0);
        assert!(worst <= 1, "grain {grain}: the export's luma is {worst} levels from the renderer's");
        planes_at.push(got.to_vec());
    }
    let apart = planes_at[0].iter().zip(&planes_at[1]).filter(|(a, b)| a.abs_diff(**b) > 1).count();
    assert!(apart > planes_at[0].len() / 10, "the control: the grain is in only {apart} of the export's luma samples");
}

/// Equalize builds its histogram on the GPU in the chain (#1215): the
/// export's GPU path draws an equalised gradient as the renderer draws it,
/// and visibly equalised (the control).
///
/// Unequalised, the two are within a level everywhere. Equalising
/// stretches the picture's levels apart, so on this small frame (its 512
/// pixels a few to a bin) a sample a level apart before can land a bin
/// over and come out a few levels apart: a handful of samples, bounded
/// here, the same with the CDF built on the CPU.
#[test]
fn an_equalised_gradient_exports_as_the_renderer_draws_it() {
    let gradient = clip(ClipKind::Generator(GeneratorData::LivingGradient { a: [12, 23, 32, 255], b: [120, 160, 90, 255], angle_deg: 25.0, drift_deg_per_sec: 0.0, glow: 0.0, grain: 0.0 }), 0.0, 4.0);
    let mut luma = Vec::new();
    for fx in [vec![], vec![BuiltinEffect::Equalize { strength: 1.0 }]] {
        let seq = sequence(vec![vec![TestClip::of(gradient.clone()).effects(fx.clone()).build()]]);
        let mut src = Flat::default();
        let Some(planes) = gpu_planes(&seq, 1.0, &mut src) else {
            no_gpu("the GPU path refused an equalised generator");
            return;
        };
        let want = cpu_luma(&seq, 1.0, &mut src);
        let got = &planes[..want.len()];
        let worst = got.iter().zip(&want).map(|(g, w)| g.abs_diff(*w)).max().unwrap_or(0);
        let over_a_level = got.iter().zip(&want).filter(|(g, w)| g.abs_diff(**w) > 1).count();
        let (most_off, most_samples) = if fx.is_empty() { (1, 0) } else { (4, want.len() / 50) };
        assert!(worst <= most_off && over_a_level <= most_samples, "{fx:?}: the export's luma is up to {worst} levels from the renderer's, {over_a_level} samples over a level");
        luma.push(got.to_vec());
    }
    let apart = luma[0].iter().zip(&luma[1]).filter(|(a, b)| a.abs_diff(**b) > 2).count();
    assert!(apart > luma[0].len() / 4, "the control: equalising changed only {apart} of the export's luma samples");
}

/// A grade keyed over the shot (#891) exports the frame it is at: its
/// exposure keyed from 0 to 2 stops, the GPU path's luma at two times is
/// the renderer's own at each, and the two times differ, so the export
/// reads the keys where the viewer does.
#[test]
fn a_keyed_grade_exports_each_frame_as_the_renderer_draws_it() {
    let mut c = graded(0.0, 2.0, 0.0);
    c.edit_grade(|g| {
        let node = g.nodes.iter_mut().find(|n| n.kind == NodeKind::Exposure).expect("the Exposure node");
        let keys = &mut node.keyframes.track_mut("exposure").keyframes;
        klipp_core::keyframe::upsert_keyframe(keys, 0.0, 0.0);
        klipp_core::keyframe::upsert_keyframe(keys, 2.0, 2.0);
    });
    let seq = sequence(vec![vec![c.clone()]]);
    if !gpu_path_available() {
        no_gpu("the GPU path refused (no usable GPU)");
        return;
    }
    let mut means = Vec::new();
    for t in [0.5, 1.5] {
        let mut src = Flat::default().with(&c, [46, 128, 230]).on_gpu();
        // The path takes a plain graded clip here, so it must take this one.
        let planes = gpu_planes(&seq, t, &mut src).unwrap_or_else(|| panic!("at {t} s: the GPU path refused a keyed grade"));
        let want = cpu_luma(&seq, t, &mut src);
        let got = &planes[..want.len()];
        let worst = got.iter().zip(want.iter()).map(|(g, w)| (*g as i32 - *w as i32).abs()).max().unwrap_or(0);
        assert!(worst <= 1, "at {t} s: luma differs from the renderer by {worst} levels");
        means.push(got.iter().map(|&v| f64::from(v)).sum::<f64>() / got.len() as f64);
    }
    assert!(means[1] > means[0] + 10.0, "the keys didn't move the exported picture: mean luma {means:?}");
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
    assert!(c.grade().is_identity(), "the fixture must be ungraded");
    let seq = sequence(vec![vec![c.clone()]]);
    let mut src = Flat::default().with(&c, [46, 128, 230]).on_gpu();

    let Some(planes) = gpu_planes(&seq, 0.5, &mut src) else {
        no_gpu("the GPU path refused (no usable GPU)");
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

/// An ungraded frame decoded to memory — a reversed clip's, read
/// backwards on the CPU — is uploaded and exported on the GPU as the
/// renderer draws it. It was refused: every reversed frame of an export
/// went down the CPU composite while playback kept it on the GPU (#3220).
#[test]
fn an_ungraded_frame_in_memory_exports_on_the_gpu() {
    let c = media(0.0, 1.0);
    let seq = sequence(vec![vec![c.clone()]]);
    let mut src = Flat::default().with(&c, [46, 128, 230]);
    let mut r = renderer();
    let Some((planes, _)) = r.render_yuv420_on_gpu(&seq, tc(0.5), W, H, &mut src, RenderOptions::default(), plain_target()) else {
        if r.last_decline() == Some("the GPU pipeline was unavailable") {
            no_gpu("no usable GPU");
            return;
        }
        panic!("the GPU path refused a frame in memory: {:?}", r.last_decline());
    };
    let want = cpu_luma(&seq, 0.5, &mut src);
    let got = &planes[..want.len()];
    let worst = got.iter().zip(&want).map(|(g, w)| (*g as i32 - *w as i32).abs()).max().unwrap_or(0);
    assert!(worst <= 1, "luma differs from the renderer by {worst} levels");
    assert!(got.iter().any(|&v| v > 40), "the fixture rendered black, so agreement proves nothing");
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
        no_gpu("the GPU path refused (no usable GPU)");
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

/// Everything the single-clip path refuses, the GPU stack takes — and
/// its planes carry the renderer's picture. Before the stack existed
/// this test held that each of these shapes was *refused*, because the
/// export would otherwise have written the uncomposited frame; the
/// danger has not changed, only the answer, so now each shape has to
/// agree with the renderer instead.
///
/// A clip that is nothing but a matte source, and an empty frame, are
/// the two the stack still has nothing to blend for; they may decline.
#[test]
fn the_stack_takes_what_the_single_clip_path_refuses() {
    // Every clip here is graded, so the single-clip path's gate — not
    // its identity pass-through — is what hands them on.
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
    f = TestClip::of(f).effects([BuiltinEffect::Opacity { value: 0.5 }]).build();
    let faded_clip = sequence(vec![vec![f.clone()]]);

    // A builtin effect that is not opacity.
    let mut g = graded(0.0, 1.0, 0.5);
    g = TestClip::of(g).effects([BuiltinEffect::GaussianBlur { radius: 3.0 }]).build();
    let blurred = sequence(vec![vec![g.clone()]]);

    // A blend mode other than Normal, over a second layer.
    let mut h = graded(0.0, 1.0, 0.5);
    h.blend_mode = BlendMode::Screen;
    let under = graded(0.0, 1.0, 0.0);
    let screened = sequence(vec![vec![under.clone()], vec![h.clone()]]);
    let screened_src = Flat::default().with(&under, [60, 120, 180]).with(&h, [128, 128, 128]);

    // A clip that is some other clip's matte, with nothing to matte.
    let mut i = graded(0.0, 1.0, 0.5);
    i.consumed_as_matte = true;
    let matte_source = sequence(vec![vec![i.clone()]]);

    // A transform: a zoom resamples the frame after the grade.
    let mut j = graded(0.0, 1.0, 0.5);
    j = TestClip::of(j).effects([BuiltinEffect::Transform {
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
    }]).build();
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

    // (what, sequence, source, whether the stack must take it)
    let cases: Vec<(&str, Sequence, Flat, bool)> = vec![
        ("an overlay layer", overlay, overlay_src, true),
        ("a dissolve", with_transition, transition_src, true),
        ("track opacity 0.5", faded_track, Flat::default().with(&e, [128, 128, 128]), true),
        ("a clip opacity effect", faded_clip, Flat::default().with(&f, [128, 128, 128]), true),
        ("a gaussian blur", blurred, Flat::default().with(&g, [128, 128, 128]), true),
        ("a screen blend", screened, screened_src, true),
        ("a matte source", matte_source, Flat::default().with(&i, [128, 128, 128]), false),
        ("a zoom", zoomed, Flat::default().with(&j, [128, 128, 128]), true),
        ("a generator", generated, Flat::default(), true),
        // Drawn on the GPU as the clear canvas, graded and finished (#2377).
        ("nothing on screen", empty, Flat::default(), true),
    ];

    let mut any = false;
    for (what, seq, mut src, must) in cases {
        let Some(planes) = gpu_planes(&seq, 0.5, &mut src.clone_for_gpu()) else {
            assert!(!must, "the GPU stack declined {what}, which it composites for the viewer");
            continue;
        };
        any = true;
        let want = cpu_luma(&seq, 0.5, &mut src);
        let got = &planes[..want.len()];
        let worst = got.iter().zip(want.iter()).map(|(g, w)| (*g as i32 - *w as i32).abs()).max().unwrap_or(0);
        assert!(worst <= 1, "{what}: the stack's export luma differs from the renderer by {worst} levels");
    }
    if !any {
        no_gpu("the GPU stack declined every shape (no usable GPU?)");
    }
}

/// #1279: in an ACEScg and a BT.2020 project the stack's export planes are
/// the renderer's, through the project's own export transform (the ACES
/// Rec.709 output transform, BT.2020's gamut map): an overlay, a dissolve,
/// a screen blend, and a generator authored in BT.709.
#[test]
fn the_stack_exports_a_wide_working_space_as_the_renderer_does() {
    use klipp_core::WorkingColorspace as Ws;
    let aces = klipp_effects::gpu::Yuv420Target { tone_map_op: 3, odt_tag: klipp_color::aces::GpuOdt::Rec709Sdr.tag(), ..plain_target() };
    for (ws, target) in [(Ws::AcesCG, aces), (Ws::Bt2020Linear, bt2020_target())] {
        let (a, b) = (graded(0.0, 1.0, 0.5), graded(0.0, 1.0, 0.5));
        let overlay = sequence(vec![vec![a.clone()], vec![b.clone()]]);
        let overlay_src = Flat::default().with(&a, [80, 160, 220]).with(&b, [220, 90, 40]);
        let (c, d) = (graded(0.0, 1.0, 0.5), graded(0.5, 1.5, 0.5));
        let mut dissolve = sequence(vec![vec![c.clone(), d.clone()]]);
        dissolve.tracks[0].transitions = vec![Transition {
            id: TransitionId::new(),
            kind: klipp_core::TransitionKind::CrossDissolve,
            duration: tc(0.25),
            clip_a_id: c.id,
            clip_b_id: d.id,
            user_params: Default::default(),
            alignment: klipp_core::TransitionAlignment::Center,
        }];
        let dissolve_src = Flat::default().with(&c, [144, 224, 240]).with(&d, [240, 120, 60]);
        let mut h = graded(0.0, 1.0, 0.5);
        h.blend_mode = BlendMode::Screen;
        let under = graded(0.0, 1.0, 0.0);
        let screened = sequence(vec![vec![under.clone()], vec![h.clone()]]);
        let screened_src = Flat::default().with(&under, [60, 120, 180]).with(&h, [128, 128, 128]);
        let k = clip(ClipKind::Generator(GeneratorData::SolidColor { color: [255, 0, 0, 255] }), 0.0, 1.0);
        let generated = sequence(vec![vec![k.clone()]]);
        let cases: Vec<(&str, Sequence, Flat)> = vec![
            ("an overlay layer", overlay, overlay_src),
            ("a dissolve", dissolve, dissolve_src),
            ("a screen blend", screened, screened_src),
            ("a generator", generated, Flat::default()),
        ];
        for (what, seq, mut src) in cases {
            let Some(planes) = gpu_planes_ws(&seq, 0.5, &mut src.clone_for_gpu(), ws, target) else {
                no_gpu("the GPU path refused (no usable GPU)");
                return;
            };
            let want = cpu_luma_ws(&seq, 0.5, &mut src, ws);
            let got = &planes[..want.len()];
            let worst = got.iter().zip(want.iter()).map(|(g, w)| (*g as i32 - *w as i32).abs()).max().unwrap_or(0);
            assert!(worst <= 1, "{ws:?}, {what}: the stack's export luma differs from the renderer by {worst} levels");
        }
    }
}

/// `px` encoded as the CPU export encodes an HDR delivery in `ws`: 10-bit
/// luma at 16/219, and the frame's brightest and summed light in nits.
fn hdr_luma(px: &[f32], ws: klipp_core::WorkingColorspace, mode: klipp_core::HdrMode, peak: f32) -> (Vec<u16>, f32, f64) {
    let display = klipp_color::display::ExportDisplay::hdr(ws, mode, peak);
    let k = [0.2627f32, 0.6780, 0.0593];
    let (mut max, mut sum) = (0.0f32, 0.0f64);
    let luma = px
        .chunks_exact(4)
        .map(|p| {
            let (e, nits) = display.encode_light([p[0], p[1], p[2]]);
            max = max.max(nits);
            sum += nits as f64;
            (((16.0 + (k[0] * e[0] + k[1] * e[1] + k[2] * e[2]) * 219.0) * 4.0) + 0.5).clamp(0.0, 1023.0) as u16
        })
        .collect();
    (luma, max, sum)
}

/// #1279: an HDR delivery's stack is the renderer's frame: each
/// scene-referred layer's own display step in the delivery's display
/// light, the layers composited in it, PQ or HLG encoded at the frame's
/// peak. Two SDR layers, a scene-referred layer over an SDR one, and a
/// dissolve from a scene-referred clip, in a Rec.709 and a BT.2020
/// project, luma within a level of 1023 and, for PQ, the frame's light
/// within 1e-4. The control: the scene layer given its SDR step instead
/// is more than a level off, so the bar sees display light.
#[test]
fn the_stack_takes_an_hdr_delivery_as_the_renderer_does() {
    use klipp_color::hdr::HdrTransfer;
    use klipp_core::{HdrMode, WorkingColorspace as Ws};
    if !gpu_path_available() {
        no_gpu("no usable GPU");
        return;
    }
    let hlg_target = |peak: f32| klipp_effects::gpu::Yuv420Target { hdr_op: 2, ..pq_target(peak) };
    for ws in [Ws::Rec709Linear, Ws::Bt2020Linear] {
        for (mode, target) in [(HdrMode::Pq, pq_target(1000.0)), (HdrMode::Hlg, hlg_target(1000.0))] {
            let (a, b) = (graded(0.0, 1.0, 0.5), graded(0.0, 1.0, 0.5));
            let overlay = sequence(vec![vec![a.clone()], vec![b.clone()]]);
            let overlay_src = Flat::default().with(&a, [80, 160, 220]).with(&b, [220, 90, 40]);
            let (base, top) = (graded(0.0, 1.0, 0.0), graded(0.0, 1.0, 0.5));
            let mut top = TestClip::of(top).effects([BuiltinEffect::Opacity { value: 0.6 }]).build();
            top.blend_mode = BlendMode::Screen;
            // The first track is the top one.
            let scene_over = sequence(vec![vec![top.clone()], vec![base.clone()]]);
            let scene_over_src = Flat::default().with(&base, [60, 120, 180]).scene_in(&top, HdrTransfer::Log(klipp_core::ColorTransfer::SLog3), SAME_PRIMARIES);
            let (c, d) = (graded(0.0, 1.0, 0.5), graded(0.5, 1.5, 0.5));
            let mut dissolve = sequence(vec![vec![c.clone(), d.clone()]]);
            dissolve.tracks[0].transitions = vec![Transition {
                id: TransitionId::new(),
                kind: klipp_core::TransitionKind::CrossDissolve,
                duration: tc(0.25),
                clip_a_id: c.id,
                clip_b_id: d.id,
                user_params: Default::default(),
                alignment: klipp_core::TransitionAlignment::Center,
            }];
            let dissolve_src = Flat::default().scene_in(&c, HdrTransfer::Pq, SAME_PRIMARIES).with(&d, [240, 120, 60]);
            let opts = RenderOptions { hdr_peak_nits: Some(1000.0), ..Default::default() };
            for (what, seq, mut src, scene) in [("two SDR layers", overlay, overlay_src, false), ("a log layer over an SDR one", scene_over, scene_over_src, true), ("a dissolve from PQ", dissolve, dissolve_src, true)] {
                let case = format!("{ws:?}, {mode:?}, {what}");
                let mut r = renderer();
                r.set_working_colorspace(ws);
                let (planes, light) = r
                    .render_yuv420_on_gpu(&seq, tc(0.5), W, H, &mut src.clone_for_gpu(), opts, target)
                    .unwrap_or_else(|| panic!("{case}: the GPU path refused: {:?}", r.last_decline()));
                let mut cpu = renderer();
                cpu.set_working_colorspace(ws);
                let (want, max, sum) = hdr_luma(&cpu.cpu_render(&seq, tc(0.5), W, H, &mut src, opts), ws, mode, 1000.0);
                let got: Vec<u16> = planes[..want.len() * 2].chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]]) >> 6).collect();
                let worst = |a: &[u16], b: &[u16]| a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0);
                let w = worst(&got, &want);
                assert!(w <= 1, "{case}: the stack's luma differs from the renderer by {w} of 1023");
                assert!(got.iter().any(|&v| v > 200), "{case}: rendered black");
                if mode == HdrMode::Pq {
                    let l = light.expect("a PQ delivery comes back with its light");
                    assert!((l.max_nits - max).abs() <= max * 1e-4, "{case}: MaxCLL {} vs {max}", l.max_nits);
                    let (gm, cm) = (l.sum_nits / l.pixels as f64, sum / l.pixels as f64);
                    assert!((gm - cm).abs() <= 0.01 + cm.abs() * 1e-4, "{case}: MaxFALL {gm} vs {cm}");
                }
                if scene {
                    let mut sdr = renderer();
                    sdr.set_working_colorspace(ws);
                    let (off, _, _) = hdr_luma(&sdr.cpu_render(&seq, tc(0.5), W, H, &mut src, RenderOptions::default()), ws, mode, 1000.0);
                    assert!(worst(&got, &off) > 1, "{case}: the SDR step is within a level, so the bar can't see display light");
                }
            }
        }
    }
}

/// #1723: a title's white in an HDR delivery is the sequence's title
/// brightness, held under the peak, and the stack draws it as the
/// renderer does. A white box filling the frame: 600 nits is display
/// light 600/203 everywhere, 2000 nits on a 1000-nit master is the peak,
/// and the default is SDR white.
#[test]
fn a_titles_hdr_brightness_is_the_renderers_on_the_stack() {
    use klipp_timeline::{Shape, ShapeKind, TitleData, TitleObjectContent, TitlePosition};
    if !gpu_path_available() {
        no_gpu("no usable GPU");
        return;
    }
    let mut td = TitleData::default();
    td.objects.clear();
    td.add_object(
        TitleObjectContent::Shape(Shape { kind: ShapeKind::Rectangle { corner_radius: 0.0 }, fill: Some([255; 4]), gradient: None, stroke: None, fit: None }),
        klipp_timeline::ObjectTransform { center: TitlePosition { x: 0.5, y: 0.5 }, size: TitlePosition { x: 1.2, y: 1.2 }, ..klipp_timeline::default_text_transform() },
    );
    let title = clip(ClipKind::Title(td), 0.0, 1.0);
    let opts = RenderOptions { hdr_peak_nits: Some(1000.0), ..Default::default() };
    for (nits, want) in [(203.0, 1.0), (600.0, 600.0 / 203.0), (2000.0, 1000.0 / 203.0)] {
        let mut seq = sequence(vec![vec![title.clone()]]);
        seq.title_nits = nits;
        let mut cpu = renderer();
        let px = cpu.cpu_render(&seq, tc(0.5), W, H, &mut Flat::default(), opts);
        let mid = &px[((H / 2 * W + W / 2) * 4) as usize..][..4];
        assert!(mid[..3].iter().all(|v| (v - want).abs() <= want * 1e-3), "{nits} nits: the renderer drew {mid:?}, want {want}");
        let (want_luma, _, _) = hdr_luma(&px, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::HdrMode::Pq, 1000.0);
        let mut r = renderer();
        let (planes, _) = r
            .render_yuv420_on_gpu(&seq, tc(0.5), W, H, &mut Flat::default(), opts, pq_target(1000.0))
            .unwrap_or_else(|| panic!("{nits} nits: the GPU path refused: {:?}", r.last_decline()));
        let got: Vec<u16> = planes[..want_luma.len() * 2].chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]]) >> 6).collect();
        let worst = got.iter().zip(&want_luma).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
        assert!(worst <= 1, "{nits} nits: the stack differs from the renderer by {worst} of 1023");
    }
    // An SDR render has no say in it.
    let mut seq = sequence(vec![vec![title]]);
    seq.title_nits = 600.0;
    let px = renderer().cpu_render(&seq, tc(0.5), W, H, &mut Flat::default(), RenderOptions::default());
    assert!(px[..3].iter().all(|v| (v - 1.0).abs() < 1e-3), "SDR: {:?}", &px[..4]);
}

/// A clip whose height is keyed apart from its width (#1471): halfway
/// between its keys the width's lane gives 1 and the height's 0.5, so
/// the picture fills the frame's width and the middle half of its rows.
/// The renderer draws it so, and the stack draws what the renderer does.
#[test]
fn a_height_keyed_apart_agrees_with_the_renderer() {
    let c = TestClip::of(graded(0.0, 1.0, 0.5)).lane("scale", 0.0, 0.5).lane("scale", 1.0, 1.5).lane("scale_y", 0.0, 0.25).lane("scale_y", 1.0, 0.75).build();
    let seq = sequence(vec![vec![c.clone()]]);
    let mut src = Flat::default().with(&c, [200, 200, 200]);
    let want = cpu_luma(&seq, 0.5, &mut src);
    let at = |x: u32, y: u32| want[(y * W + x) as usize];
    // Bars above and below the middle half; the picture edge to edge in it.
    for (x, y) in [(W / 2, 1), (W / 2, H - 2)] {
        assert!(at(x, y) <= 17, "({x}, {y}) is {}: no bar where the height's lane leaves one", at(x, y));
    }
    for (x, y) in [(0, H / 2), (W / 2, H / 2), (W - 1, H / 2)] {
        assert!(at(x, y) > 100, "({x}, {y}) is {}: no picture where the width's lane puts it", at(x, y));
    }
    let Some(planes) = gpu_planes(&seq, 0.5, &mut src.clone_for_gpu()) else {
        no_gpu("the stack declined (no usable GPU?)");
        return;
    };
    let worst = planes[..want.len()].iter().zip(&want).map(|(g, w)| (*g as i32 - *w as i32).abs()).max().unwrap_or(0);
    assert!(worst <= 1, "the stack's luma differs from the renderer by {worst} levels");
}

/// A composite with the timeline's LUT over it: the LUT runs on the
/// stack's accumulator, on the same side of the premultiply as the
/// renderer puts it.
#[test]
fn a_global_lut_over_a_composite_agrees_with_the_renderer() {
    let (a, b) = (graded(0.0, 1.0, 0.3), graded(0.0, 1.0, -0.2));
    let mut b = b;
    b.blend_mode = BlendMode::Screen;
    b = TestClip::of(b).effects([BuiltinEffect::Opacity { value: 0.6 }]).build();
    let seq = sequence(vec![vec![a.clone()], vec![b.clone()]]);
    let mut src = Flat::default().with(&a, [190, 110, 70]).with(&b, [40, 160, 220]).on_gpu();
    let lut = strong_lut();

    let mut r = renderer();
    r.set_global_lut(Some(klipp_compose::GlobalLut { lut: lut.clone(), opacity: 0.8 }));
    let Some((planes, _)) =
        r.render_yuv420_on_gpu(&seq, tc(0.5), W, H, &mut src, RenderOptions::default(), plain_target())
    else {
        no_gpu("the GPU stack declined (no usable GPU?)");
        return;
    };
    let mut rr = renderer();
    rr.set_global_lut(Some(klipp_compose::GlobalLut { lut, opacity: 0.8 }));
    let px = rr.cpu_render(&seq, tc(0.5), W, H, &mut src, RenderOptions::default());
    let display = klipp_color::display::ExportDisplay::new(
        klipp_core::ToneMapOp::None,
        klipp_core::WorkingColorspace::Rec709Linear,
        klipp_core::OdtChoice::MatchViewer,
    );
    let want: Vec<u8> = px
        .chunks_exact(4)
        .map(|p| {
            let e = display.encode([p[0], p[1], p[2]]);
            let y = LUMA_709[0] * e[0] + LUMA_709[1] * e[1] + LUMA_709[2] * e[2];
            (16.0 + y * 219.0 + 0.5).clamp(0.0, 255.0) as u8
        })
        .collect();
    let got = &planes[..want.len()];
    let worst = got.iter().zip(want.iter()).map(|(g, w)| (*g as i32 - *w as i32).abs()).max().unwrap_or(0);
    assert!(worst <= 1, "the LUT'd composite differs from the renderer by {worst} levels");
    let no_lut = renderer()
        .render_yuv420_on_gpu(&seq, tc(0.5), W, H, &mut src, RenderOptions::default(), plain_target())
        .expect("the same composite without a LUT")
        .0;
    let d = planes.iter().zip(no_lut.iter()).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
    assert!(d > 20, "only {d}: the LUT is not reaching the exported composite");
}

/// A single graded clip under a timeline grade: the single-clip path has
/// no composite for the timeline grade to run on, so it hands the frame
/// to the stack, and what comes out is the renderer's frame with the
/// timeline grade in it — not the clip's grade alone.
#[test]
fn a_timeline_grade_over_a_single_clip_agrees_with_the_renderer() {
    let a = graded(0.0, 1.0, 0.3);
    let mut seq = sequence(vec![vec![a.clone()]]);
    seq.timeline_grade_slot().set(Some(graded(0.0, 1.0, -1.0).grade().clone()));
    let mut src = Flat::default().with(&a, [190, 110, 70]).on_gpu();
    let Some(got) = gpu_planes(&seq, 0.5, &mut src) else {
        no_gpu("the GPU path declined (no usable GPU?)");
        return;
    };
    let want = cpu_luma(&seq, 0.5, &mut src);
    let worst = got[..want.len()].iter().zip(want.iter()).map(|(g, w)| (*g as i32 - *w as i32).abs()).max().unwrap_or(0);
    assert!(worst <= 1, "the timeline-graded frame differs from the renderer by {worst} levels");
    seq.timeline_grade_slot().set(None);
    let ungraded = gpu_planes(&seq, 0.5, &mut src).expect("the same clip without a timeline grade");
    let d = got.iter().zip(ungraded.iter()).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
    assert!(d > 20, "only {d}: the timeline grade is not reaching the export");
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
        no_gpu("the GPU path refused (no usable GPU)");
        return;
    };

    // The renderer's own frame, encoded the way the CPU export encodes a
    // PQ delivery, and its light accumulated the same way.
    let mut r = renderer();
    let px = r.cpu_render(&seq, tc(0.5), W, H, &mut src, opts);
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

/// A project-wide LUT is *applied*, and the picture matches the renderer.
///
/// It runs as one more chain pass, so the frame stays on the GPU — before
/// this a project with a look LUT exported at the float route's speed,
/// and before the refusal that preceded that, without its LUT at all.
///
/// The second half is what makes the first mean something: the same
/// fixture with no LUT has to come out *different*. A LUT that silently
/// did nothing would match a renderer that also did nothing.
#[test]
fn a_global_lut_is_applied_and_agrees_with_the_renderer() {
    let c = graded(0.0, 1.0, 0.5);
    let seq = sequence(vec![vec![c.clone()]]);
    let mut src = Flat::default().with(&c, [190, 110, 70]).on_gpu();
    let lut = strong_lut();

    let mut r = renderer();
    r.set_global_lut(Some(klipp_compose::GlobalLut { lut: lut.clone(), opacity: 1.0 }));
    let Some((planes, _)) = r.render_yuv420_on_gpu(
        &seq,
        tc(0.5),
        W,
        H,
        &mut src,
        RenderOptions::default(),
        plain_target(),
    ) else {
        no_gpu("the GPU path refused (no usable GPU)");
        return;
    };

    // The renderer's own frame, with the same LUT, encoded the same way.
    let mut rr = renderer();
    rr.set_global_lut(Some(klipp_compose::GlobalLut { lut: lut.clone(), opacity: 1.0 }));
    let px = rr.cpu_render(&seq, tc(0.5), W, H, &mut src, RenderOptions::default());
    let display = klipp_color::display::ExportDisplay::new(
        klipp_core::ToneMapOp::None,
        klipp_core::WorkingColorspace::Rec709Linear,
        klipp_core::OdtChoice::MatchViewer,
    );
    let want: Vec<u8> = px
        .chunks_exact(4)
        .map(|p| {
            let e = display.encode([p[0], p[1], p[2]]);
            let y = LUMA_709[0] * e[0] + LUMA_709[1] * e[1] + LUMA_709[2] * e[2];
            (16.0 + y * 219.0 + 0.5).clamp(0.0, 255.0) as u8
        })
        .collect();
    let got = &planes[..want.len()];
    let worst =
        got.iter().zip(want.iter()).map(|(g, w)| (*g as i32 - *w as i32).abs()).max().unwrap_or(0);
    eprintln!("global LUT export luma vs the renderer: worst {worst} of 255");
    assert!(worst <= 1, "the LUT'd frame differs from the renderer by {worst} levels");

    // The LUT reached the picture. Without this the test would pass for a
    // chain pass that did nothing at all.
    let mut plain = renderer();
    let no_lut = plain
        .render_yuv420_on_gpu(
            &seq,
            tc(0.5),
            W,
            H,
            &mut src,
            RenderOptions::default(),
            plain_target(),
        )
        .expect("the same frame without a LUT")
        .0;
    // Whole planes, not just luma: a look LUT moves chroma more than it
    // moves brightness, so Cb and Cr are where the difference lives.
    let d = planes
        .iter()
        .zip(no_lut.iter())
        .map(|(a, b)| (*a as i32 - *b as i32).abs())
        .max()
        .unwrap_or(0);
    eprintln!("with the LUT vs without: worst {d} of 255 over the whole planes");
    assert!(d > 20, "only {d}: the LUT is not reaching the exported picture");
}

/// Where the LUT's position is ambiguous, the path declines instead of
/// putting it on the wrong side of something.
#[test]
fn a_global_lut_is_refused_where_its_position_is_ambiguous() {
    use klipp_color::hdr::HdrTransfer;
    let c = graded(0.0, 1.0, 0.5);
    let seq = sequence(vec![vec![c.clone()]]);
    let lut = strong_lut();

    // An HDR delivery splits the light above SDR white around the LUT,
    // and that split has no chain pass.
    let mut src = Flat::default().with(&c, [190, 110, 70]).on_gpu();
    let mut r = renderer();
    r.set_global_lut(Some(klipp_compose::GlobalLut { lut: lut.clone(), opacity: 1.0 }));
    assert!(
        r.render_yuv420_on_gpu(
            &seq,
            tc(0.5),
            W,
            H,
            &mut src,
            RenderOptions { hdr_peak_nits: Some(1000.0), ..Default::default() },
            pq_target(1000.0),
        )
        .is_none(),
        "an HDR delivery with a global LUT must decline",
    );

    // A scene-referred clip ends in a display pass the LUT goes after,
    // and the fused writer runs that pass last.
    let mut scene_src = Flat::default().scene(&c, HdrTransfer::Pq);
    let mut r2 = renderer();
    r2.set_global_lut(Some(klipp_compose::GlobalLut { lut, opacity: 1.0 }));
    assert!(
        r2.render_yuv420_on_gpu(
            &seq,
            tc(0.5),
            W,
            H,
            &mut scene_src,
            RenderOptions::default(),
            plain_target(),
        )
        .is_none(),
        "a scene-referred clip with a global LUT must decline",
    );

    // And both shapes are accepted without the LUT, so the refusals above
    // are the LUT and not the fixtures.
    let mut ok1 = renderer();
    assert!(
        ok1.render_yuv420_on_gpu(
            &seq,
            tc(0.5),
            W,
            H,
            &mut scene_src,
            RenderOptions::default(),
            plain_target(),
        )
        .is_some(),
        "the scene-referred fixture cannot take the path at all",
    );
}

/// A LUT with real cross-talk, so trilinear and tetrahedral differ and a
/// pass-through is obvious.
fn strong_lut() -> klipp_color::lut::LutData {
    let size = 5usize;
    let step = 1.0 / (size - 1) as f32;
    let mut data = Vec::with_capacity(size * size * size * 3);
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                let (rv, gv, bv) = (r as f32 * step, g as f32 * step, b as f32 * step);
                data.extend_from_slice(&[
                    (rv * 0.5 + 0.4 * gv * bv).clamp(0.0, 1.0),
                    (gv * 0.7 + 0.2 * rv).clamp(0.0, 1.0),
                    (bv * 0.45 + 0.3 * rv * gv).clamp(0.0, 1.0),
                ]);
            }
        }
    }
    klipp_color::lut::LutData {
        size,
        data,
        input_space: klipp_core::WorkingColorspace::Rec709Linear,
        output_space: klipp_core::WorkingColorspace::Rec709Linear,
    }
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
            OdtChoice::Rec2020Sdr,
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
                no_gpu("the GPU path refused (no usable GPU)");
                return;
            };
            assert!(light.is_none(), "{odt:?}: an ODT delivery measures no light");

            // The renderer's frame through the same ExportDisplay.
            let mut r = renderer();
            r.set_working_colorspace(working);
            let px = r.cpu_render(&seq, tc(0.5), W, H, &mut src, RenderOptions::default());
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
    // blob's variant and ran one chain for all five.
    for (i, a) in [
        OdtChoice::Rec709Sdr,
        OdtChoice::DciP3D65_48,
        OdtChoice::Rec2100Pq1000,
        OdtChoice::Rec2100Pq4000,
        OdtChoice::Rec2020Sdr,
    ]
    .iter()
    .enumerate()
    {
        for b in [
            OdtChoice::Rec709Sdr,
            OdtChoice::DciP3D65_48,
            OdtChoice::Rec2100Pq1000,
            OdtChoice::Rec2100Pq4000,
            OdtChoice::Rec2020Sdr,
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

/// A *scene-referred* source through the whole path — which is what a
/// real HDR export is.
///
/// PQ, HLG and log footage ends in its own display pass after the grade:
/// an HDR delivery's display light, or an SDR delivery's filmic tone map.
/// That pass had no shader twin, so this was the case the gate refused
/// even after the PQ and HLG *encodes* were on the GPU — and it is the
/// one every real HDR export takes, since the source of an HDR master is
/// HDR footage.
///
/// Both deliveries, and all three transfers, because the display-light
/// derivation is a different expression for each: PQ is a scale, HLG is
/// a luma-weighted OOTF, and a camera log is the BT.2100 reference OOTF
/// with a highlight roll-off against the master's peak.
#[test]
fn a_scene_referred_source_agrees_with_the_renderer() {
    use klipp_color::hdr::HdrTransfer;
    use klipp_core::WorkingColorspace as W;

    let transfers = [
        ("PQ", HdrTransfer::Pq),
        ("HLG", HdrTransfer::Hlg),
        ("S-Log3", HdrTransfer::Log(klipp_core::ColorTransfer::SLog3)),
    ];
    if !gpu_path_available() {
        no_gpu("no usable GPU");
        return;
    }
    let cases = transfers.into_iter().flat_map(|t| {
        [false, true].into_iter().flat_map(move |hdr| {
            [true, false].into_iter().flat_map(move |grade| [("BT.709", SAME_PRIMARIES), ("BT.2020", BT2020_TO_709)].into_iter().map(move |p| (t, hdr, grade, p)))
        })
    });
    for ((name, transfer), hdr, grade, (primaries, to_working)) in cases {
        {
            // An ungraded clip takes the identity branch, which seeds the
            // chain from the codes too (#1275); a graded one runs its graph.
            let c = if grade { graded(0.0, 1.0, 0.5) } else { media(0.0, 1.0) };
            let name = format!("{name} in {primaries}, {}", if grade { "graded" } else { "ungraded" });
            let name = name.as_str();
            let seq = sequence(vec![vec![c.clone()]]);
            let mut src = Flat::default().scene_in(&c, transfer, to_working);
            let opts = RenderOptions {
                hdr_peak_nits: hdr.then_some(1000.0),
                ..Default::default()
            };
            let target = if hdr { pq_target(1000.0) } else { plain_target() };

            let mut r = renderer();
            // With a GPU, every case takes the path: an ungraded clip
            // refused here is the bug this case is for (#1275).
            let (planes, light) = r
                .render_yuv420_on_gpu(&seq, tc(0.5), W, H, &mut src, opts, target)
                .unwrap_or_else(|| panic!("{name}: the GPU path refused: {:?}", r.last_decline()));

            // The renderer's own frame, encoded as the CPU export encodes it.
            let mut rr = renderer();
            let px = rr.cpu_render(&seq, tc(0.5), W, H, &mut src, opts);
            let display = if hdr {
                klipp_color::display::ExportDisplay::hdr(
                    W::Rec709Linear,
                    klipp_core::HdrMode::Pq,
                    1000.0,
                )
            } else {
                klipp_color::display::ExportDisplay::new(
                    klipp_core::ToneMapOp::None,
                    W::Rec709Linear,
                    klipp_core::OdtChoice::MatchViewer,
                )
            };
            let k = if hdr { [0.2627f32, 0.6780, 0.0593] } else { LUMA_709 };
            let (scale, top) = if hdr { (4.0f32, 1023.0f32) } else { (1.0, 255.0) };
            let mut want = Vec::with_capacity((W * H) as usize);
            let mut encoded = Vec::with_capacity((W * H) as usize);
            let (mut max, mut sum) = (0.0f32, 0.0f64);
            for p in px.chunks_exact(4) {
                let (e, nits) = display.encode_light([p[0], p[1], p[2]]);
                max = max.max(nits);
                sum += nits as f64;
                let y = k[0] * e[0] + k[1] * e[1] + k[2] * e[2];
                want.push(((16.0 + y * 219.0) * scale + 0.5).clamp(0.0, top) as u16);
                encoded.push(e);
            }
            // Chroma as the encoder makes it: each site from the mean of
            // its 2x2 quad's encoded RGB, then the difference from luma, so
            // a matrix or chroma error shared by both GPU branches shows.
            let quant = |v: f32| ((128.0 + v * 224.0) * scale + 0.5).clamp(0.0, top) as u16;
            let mut want_cb = Vec::with_capacity((W * H / 4) as usize);
            let mut want_cr = Vec::with_capacity((W * H / 4) as usize);
            for by in 0..(H / 2) as usize {
                for bx in 0..(W / 2) as usize {
                    let at = |dy: usize, dx: usize| encoded[(by * 2 + dy) * W as usize + bx * 2 + dx];
                    let mut acc = [0.0f32; 3];
                    for e in [at(0, 0), at(0, 1), at(1, 0), at(1, 1)] {
                        for c in 0..3 {
                            acc[c] += e[c] * 0.25;
                        }
                    }
                    let l = k[0] * acc[0] + k[1] * acc[1] + k[2] * acc[2];
                    want_cb.push(quant((acc[2] - l) / (2.0 * (1.0 - k[2]))));
                    want_cr.push(quant((acc[0] - l) / (2.0 * (1.0 - k[0]))));
                }
            }

            let got: Vec<u16> = if hdr {
                planes[..want.len() * 2]
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]) >> 6)
                    .collect()
            } else {
                planes[..want.len()].iter().map(|v| *v as u16).collect()
            };
            let worst = got
                .iter()
                .zip(want.iter())
                .map(|(g, w)| (*g as i32 - *w as i32).abs())
                .max()
                .unwrap_or(0);
            let delivery = if hdr { "PQ" } else { "SDR" };
            eprintln!("{name} -> {delivery}: worst {worst} of {top}");
            assert!(worst <= 1, "{name} -> {delivery}: differs by {worst} levels");

            // The chroma: P010 interleaves 16-bit Cb,Cr after the luma; the
            // plain delivery has 8-bit Cb then Cr planes.
            let sites = want_cb.len();
            let (got_cb, got_cr): (Vec<u16>, Vec<u16>) = if hdr {
                let pairs = &planes[want.len() * 2..want.len() * 2 + sites * 4];
                pairs.chunks_exact(4).map(|b| (u16::from_le_bytes([b[0], b[1]]) >> 6, u16::from_le_bytes([b[2], b[3]]) >> 6)).unzip()
            } else {
                let c = &planes[want.len()..want.len() + sites * 2];
                (c[..sites].iter().map(|v| *v as u16).collect(), c[sites..].iter().map(|v| *v as u16).collect())
            };
            let off = |g: &[u16], w: &[u16]| g.iter().zip(w).map(|(g, w)| (*g as i32 - *w as i32).abs()).max().unwrap_or(0);
            let (worst_cb, worst_cr) = (off(&got_cb, &want_cb), off(&got_cr, &want_cr));
            assert!(worst_cb <= 1 && worst_cr <= 1, "{name} -> {delivery}: chroma differs by {worst_cb} (Cb), {worst_cr} (Cr) levels");
            let centre = 128.0 * scale;
            assert!(
                want_cb.iter().chain(&want_cr).any(|&v| (v as f32 - centre).abs() > 4.0 * scale),
                "{name} -> {delivery}: the fixture is grey, so its chroma proves nothing",
            );
            assert!(
                got.iter().any(|&v| v as f32 > 16.0 * scale + 8.0),
                "{name} -> {delivery}: the fixture rendered black",
            );

            if hdr {
                let l = light.expect("a PQ delivery comes back with its light");
                let pixels = (W as u64) * (H as u64);
                assert!(
                    (l.max_nits - max).abs() <= max * 1e-4,
                    "{name}: MaxCLL is {} vs {max}",
                    l.max_nits,
                );
                let (gm, cm) = (l.sum_nits / pixels as f64, sum / pixels as f64);
                assert!((gm - cm).abs() < 0.01 + cm.abs() * 1e-5, "{name}: MaxFALL {gm} vs {cm}");
            } else {
                assert!(light.is_none(), "{name}: an SDR delivery measures no light");
            }
        }
    }
}

/// The identity branch's scene-referred seed is the graded branch's (#1275):
/// an ungraded clip and the same clip under a graph that does work and
/// cancels (a stop up, a stop down: not an identity graph, so it runs) give
/// the same planes, chroma included, in other primaries than the project's.
#[test]
fn an_ungraded_scene_referred_clip_is_its_cancelled_graded_self() {
    use klipp_color::hdr::HdrTransfer;
    if !gpu_path_available() {
        no_gpu("no usable GPU");
        return;
    }
    for transfer in [HdrTransfer::Pq, HdrTransfer::Hlg, HdrTransfer::Log(klipp_core::ColorTransfer::SLog3)] {
        let plain = media(0.0, 1.0);
        let mut cancelled = graded(0.0, 1.0, 1.0);
        {
            // A stop up, then a stop down, in series.
            let mut g = cancelled.grade().clone();
            let down = Node {
                id: NodeId::new_v4(),
                kind: NodeKind::Exposure,
                label: "Exposure".into(),
                enabled: true,
                params: NodeParams { exposure: Some(-1.0), ..Default::default() },
                keyframes: Default::default(),
                output_link: None,
                input_links: vec![],
            };
            let up = g.input_of(g.output_node_id, 0).expect("the stop up");
            g.edges.retain(|e| e.to_node != g.output_node_id);
            g.edges.push(Edge { from_node: up, from_port: 0, to_node: down.id, to_port: 0 });
            g.edges.push(Edge { from_node: down.id, from_port: 0, to_node: g.output_node_id, to_port: 0 });
            g.nodes.push(down);
            cancelled.set_grade(g);
        }
        assert!(!cancelled.grade_is_identity(), "the control's graph runs");
        let planes = |c: &Clip| {
            let seq = sequence(vec![vec![c.clone()]]);
            let mut src = Flat::default().scene_in(c, transfer, BT2020_TO_709);
            let mut r = renderer();
            let out = r.render_yuv420_on_gpu(&seq, tc(0.5), W, H, &mut src, RenderOptions::default(), plain_target());
            out.unwrap_or_else(|| panic!("{transfer:?}: refused: {:?}", r.last_decline())).0
        };
        let (a, b) = (planes(&plain), planes(&cancelled));
        let worst = a.iter().zip(&b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0);
        assert!(worst <= 1, "{transfer:?}: the identity branch differs from the cancelled grade by {worst} levels");
        assert!(a.iter().any(|&v| v > 40), "{transfer:?}: the fixture rendered black");
    }
}

/// One renderer, frame after frame of different kinds: an ungraded
/// scene-referred clip, a graded one, an SDR clip, and the ungraded one
/// again. Each is the renderer's own picture, so no seeded state from one
/// frame reaches the next.
#[test]
fn alternating_frames_on_one_renderer_keep_no_stale_seed() {
    use klipp_color::hdr::HdrTransfer;
    if !gpu_path_available() {
        no_gpu("no usable GPU");
        return;
    }
    let (ungraded, graded_clip, sdr) = (media(0.0, 1.0), graded(0.0, 1.0, 0.5), graded(0.0, 1.0, 0.3));
    let mut src = Flat::default()
        .scene_in(&ungraded, HdrTransfer::Pq, BT2020_TO_709)
        .scene(&graded_clip, HdrTransfer::Hlg)
        .with(&sdr, [200, 90, 60])
        .on_gpu();
    let mut r = renderer();
    for (what, c) in [("ungraded PQ", &ungraded), ("graded HLG", &graded_clip), ("SDR", &sdr), ("ungraded PQ again", &ungraded)] {
        let seq = sequence(vec![vec![c.clone()]]);
        let planes = r
            .render_yuv420_on_gpu(&seq, tc(0.5), W, H, &mut src, RenderOptions::default(), plain_target())
            .unwrap_or_else(|| panic!("{what}: refused: {:?}", r.last_decline()))
            .0;
        let want = cpu_luma(&seq, 0.5, &mut src);
        let worst = planes[..want.len()].iter().zip(&want).map(|(g, w)| (*g as i32 - *w as i32).abs()).max().unwrap_or(0);
        assert!(worst <= 1, "{what}: differs from the renderer by {worst} levels");
    }
}
