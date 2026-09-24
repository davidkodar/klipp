//! The rendering rules in `klipp_compose::render`, each pinned on a small
//! generated timeline. Several of these are bugs the viewer had: they
//! fail against its old composite.
//!
//! Every test renders twice, CPU paths only and GPU where available, and
//! both must give the answer.

use std::collections::HashMap;

use std::sync::Arc;

use klipp_compose::{active_layers, FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ClipId, GeneratorData, RationalTime, Timecode, TransitionId};
use klipp_effects::BuiltinEffect;
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams, NodePos};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind, Transition};

const W: u32 = 32;
const H: u32 = 18;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn clip(kind: ClipKind, from: f64, to: f64) -> Clip {
    Clip { kind, ..Clip::gap(tc(from), tc(to)) }
}

fn solid(rgb: [u8; 3], from: f64, to: f64) -> Clip {
    clip(ClipKind::Generator(GeneratorData::SolidColor { color: [rgb[0], rgb[1], rgb[2], 255] }), from, to)
}

fn media(from: f64, to: f64) -> Clip {
    clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, from, to)
}

/// Tracks given top first, as `Sequence::tracks` stores them.
fn sequence(tracks: Vec<Vec<Clip>>) -> Sequence {
    let mut seq = Sequence::new_default();
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

fn graph(kind: NodeKind, params: NodeParams, keyframes: klipp_nodes::NodeKeyframeTracks) -> NodeGraph {
    let node = |kind, params, keyframes| Node {
        id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes,
        output_link: None, input_links: vec![],
    };
    let nodes = vec![
        node(NodeKind::Source, NodeParams::default(), Default::default()),
        node(kind, params, keyframes),
        node(NodeKind::Output, NodeParams::default(), Default::default()),
    ];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
    NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

fn exposure(stops: f32) -> NodeGraph {
    let p = NodeParams {
        exposure: Some(stops),
        ..Default::default()
    };
    graph(NodeKind::Exposure, p, Default::default())
}

/// Media clips return a flat frame chosen per clip.
#[derive(Default)]
struct Flat(HashMap<ClipId, Box<dyn Fn() -> SourcePixels>>);

impl Flat {
    fn grey(mut self, c: &Clip, v: f32) -> Self {
        self.0.insert(c.id, Box::new(move || SourcePixels::Linear(Arc::new([v, v, v, 1.0].repeat((W * H) as usize)))));
        self
    }
    /// A flat colour rather than a flat grey. Needed wherever the
    /// transform under test mixes channels: on a grey, `r == g == b`, so
    /// only the *row sums* of a 3x3 are observable and its columns are
    /// not.
    fn linear(mut self, c: &Clip, rgb: [f32; 3]) -> Self {
        self.0.insert(c.id, Box::new(move || {
            SourcePixels::Linear(Arc::new([rgb[0], rgb[1], rgb[2], 1.0].repeat((W * H) as usize)))
        }));
        self
    }
    fn slog3(self, c: &Clip, code: u32) -> Self {
        self.scene(c, klipp_color::hdr::HdrTransfer::Log(klipp_core::ColorTransfer::SLog3), code)
    }
    fn scene(mut self, c: &Clip, transfer: klipp_color::hdr::HdrTransfer, code: u32) -> Self {
        let packed = klipp_color::hdr::pack_rgb10(code, code, code);
        self.0.insert(c.id, Box::new(move || SourcePixels::SceneReferred {
            codes: Arc::new(vec![packed; (W * H) as usize]),
            transfer,
            source_to_working: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }));
        self
    }
}

impl FrameSource for Flat {
    fn frame(&mut self, clip: &Clip, _source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        self.0.get(&clip.id).map(|f| SourceFrame { pixels: f(), width, height, pts: None })
    }
}

/// Render at `t` with CPU paths and with the GPU where available.
fn render(seq: &Sequence, t: f64, src: &mut Flat) -> [Vec<f32>; 2] {
    [false, true].map(|gpu| {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(gpu);
        r.render(seq, tc(t), W, H, src, RenderOptions::default())
    })
}

fn px(img: &[f32], x: u32, y: u32) -> [f32; 3] {
    let i = ((y * W + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2]]
}

fn assert_px(label: &str, got: [f32; 3], want: [f32; 3], tol: f32) {
    for c in 0..3 {
        assert!((got[c] - want[c]).abs() <= tol, "{label}: got {got:?}, want {want:?}");
    }
}

fn srgb_linear(v: u8) -> f32 {
    klipp_color::transfer::srgb_to_linear(v as f32 / 255.0)
}

/// Index 0 is the top track. Three opaque full-frame layers show the top
/// one; half its opacity shows it over the one directly beneath — never
/// the bottom. The viewer blended overlays top-first and got this wrong
/// with two or more overlays.
#[test]
fn tracks_stack_bottom_to_top() {
    let seq = sequence(vec![
        vec![solid([0, 0, 255], 0.0, 2.0)], // V3, top
        vec![solid([0, 255, 0], 0.0, 2.0)], // V2
        vec![solid([255, 0, 0], 0.0, 2.0)], // V1, bottom
    ]);
    for (i, img) in render(&seq, 1.0, &mut Flat::default()).iter().enumerate() {
        assert_px(&format!("opaque, run {i}"), px(img, 16, 9), [0.0, 0.0, 1.0], 1e-3);
    }
    let mut seq = seq;
    seq.tracks[0].opacity = 0.5;
    for (i, img) in render(&seq, 1.0, &mut Flat::default()).iter().enumerate() {
        assert_px(&format!("top at half, run {i}"), px(img, 16, 9), [0.0, 0.5, 0.5], 1e-3);
    }
}

/// Each clip is graded with its own graph. V1 is brightened a stop; V2,
/// shrunk to the centre and ungraded, must stay as it is, and V1 around it
/// must show V1's grade. The viewer applied the topmost clip's grade to
/// the primary track's pixels.
#[test]
fn each_clip_gets_its_own_grade() {
    let mut v1 = media(0.0, 2.0);
    v1.node_graph = exposure(1.0);
    let mut v2 = media(0.0, 2.0);
    v2.builtin_effects = vec![BuiltinEffect::Transform {
        position_x: 0.0, position_y: 0.0, scale_x: 0.5, scale_y: 0.5, rotation: 0.0,
        anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
    }];
    let mut src = Flat::default().grey(&v1, 0.2).grey(&v2, 0.2);
    let seq = sequence(vec![vec![v2], vec![v1]]);
    for (i, img) in render(&seq, 1.0, &mut src).iter().enumerate() {
        assert_px(&format!("V2 in the centre, run {i}"), px(img, 16, 9), [0.2; 3], 0.01);
        assert_px(&format!("V1 at the edge, run {i}"), px(img, 1, 1), [0.4; 3], 0.01);
    }
}

/// Opacity is one compositing weight, applied once. White at half
/// opacity over black is 0.5 in linear light — whether the half comes
/// from the Opacity effect or the track — and the two multiply. The
/// Opacity effect used to premultiply into a straight-alpha blend, and
/// overlays applied it again at the blend: 0.25, or 0.125.
#[test]
fn opacity_is_applied_once() {
    let with = |effect: Option<f32>, track: f32| {
        let mut top = solid([255, 255, 255], 0.0, 2.0);
        if let Some(v) = effect {
            top.builtin_effects = vec![BuiltinEffect::Opacity { value: v }];
        }
        let mut seq = sequence(vec![vec![top], vec![solid([0, 0, 0], 0.0, 2.0)]]);
        seq.tracks[0].opacity = track;
        render(&seq, 1.0, &mut Flat::default())
    };
    for (label, effect, track, want) in [
        ("Opacity effect 0.5", Some(0.5), 1.0, 0.5),
        ("track opacity 0.5", None, 0.5, 0.5),
        ("both", Some(0.5), 0.5, 0.25),
    ] {
        for (i, img) in with(effect, track).iter().enumerate() {
            assert_px(&format!("{label}, run {i}"), px(img, 16, 9), [want; 3], 1e-3);
        }
    }
    // Alone on the timeline, a half-transparent clip is flattened over
    // black: the export and the viewer see the same 0.5.
    let mut only = solid([255, 255, 255], 0.0, 2.0);
    only.builtin_effects = vec![BuiltinEffect::Opacity { value: 0.5 }];
    for (i, img) in render(&sequence(vec![vec![only]]), 1.0, &mut Flat::default()).iter().enumerate() {
        assert_px(&format!("alone, run {i}"), px(img, 16, 9), [0.5; 3], 1e-3);
        assert_eq!(img[3], 1.0);
    }
}

/// A fade to colour at the end of a clip is timed on the clip's length
/// on the timeline. It was timed on the source length: a clip playing
/// at 2× (four seconds of source in two of timeline) faded out at
/// source second three, which the timeline never reaches, so it ended
/// with no fade at all.
#[test]
fn a_fade_out_ends_with_the_clip_on_the_timeline() {
    let at = |t: f64| {
        let mut c = solid([255, 255, 255], 0.0, 2.0);
        c.source_in = tc(0.0);
        c.source_out = tc(4.0);
        c.builtin_effects = vec![BuiltinEffect::ColorFade {
            color: [0, 0, 0, 255],
            duration_secs: 1.0,
            mode: klipp_effects::ColorFadeMode::Out,
            progress: 1.0,
        }];
        render(&sequence(vec![vec![c]]), t, &mut Flat::default()).map(|img| px(&img, 16, 9)[0])
    };
    for (i, v) in at(0.5).iter().enumerate() {
        assert!((v - 1.0).abs() < 1e-3, "run {i}: before the fade the clip is untouched, got {v}");
    }
    let [cpu, gpu] = at(1.5);
    assert!(cpu < 0.99 && cpu > 0.01, "half-way through the fade the clip is part black, got {cpu}");
    assert!((cpu - gpu).abs() < 1e-3, "CPU {cpu} and GPU {gpu} disagree");
    let [late, _] = at(1.95);
    assert!(late < cpu, "the fade deepens towards the clip's end: {late} at 1.95 s vs {cpu} at 1.5 s");
}

/// A dissolve mixes the two clips as they are graded, in linear light,
/// both running at the frame's time. The incoming clip is brightened by
/// its own grade; mid-dissolve is half red, half that. The viewer mixed
/// against an ungraded, frozen partner.
#[test]
fn a_transition_mixes_both_graded_clips() {
    let a = solid([255, 0, 0], 0.0, 2.0);
    let mut b = media(2.0, 4.0);
    b.node_graph = exposure(1.0);
    let mut src = Flat::default().grey(&b, 0.25);
    let tr = Transition {
        id: TransitionId::new(),
        kind: klipp_core::TransitionKind::CrossDissolve,
        duration: tc(1.0),
        clip_a_id: a.id,
        clip_b_id: b.id,
        user_params: Default::default(),
        alignment: klipp_core::TransitionAlignment::Center,
    };
    let mut seq = sequence(vec![vec![a, b]]);
    seq.tracks[0].transitions.push(tr);
    for (t, p) in [(2.0, 0.5), (2.25, 0.75), (1.75, 0.25)] {
        for (i, img) in render(&seq, t, &mut src).iter().enumerate() {
            let want = [(1.0 - p) + p * 0.5, p * 0.5, p * 0.5];
            assert_px(&format!("t={t}, run {i}"), px(img, 16, 9), want, 2e-3);
        }
    }
}

/// A scene-referred clip is graded in scene-linear light and tone-mapped
/// after: S-Log3 18% grey one stop up is 36% grey through the display
/// tone map — the viewer's ungraded look, one stop brighter.
#[test]
fn a_log_clip_is_graded_scene_linear_then_tone_mapped() {
    let mut c = media(0.0, 2.0);
    let mut src = Flat::default().slog3(&c, 420);
    let tonemap = |x: f32| klipp_color::hdr::hdr_display_tonemap(x, klipp_color::hdr::LOG_DISPLAY_EXPOSURE);
    for (i, img) in render(&sequence(vec![vec![c.clone()]]), 1.0, &mut src).iter().enumerate() {
        assert_px(&format!("ungraded, run {i}"), px(img, 16, 9), [tonemap(0.18); 3], 2e-3);
    }
    c.node_graph = exposure(1.0);
    let mut src = Flat::default().slog3(&c, 420);
    for (i, img) in render(&sequence(vec![vec![c]]), 1.0, &mut src).iter().enumerate() {
        assert_px(&format!("+1 stop, run {i}"), px(img, 16, 9), [tonemap(0.36); 3], 2e-3);
    }
}

/// An HDR delivery renders display light, 1.0 being SDR white at 203
/// cd/m²: an SDR clip as it is, a PQ clip at its own luminance, S-Log3
/// grey at BT.2408's 26 nits. A global LUT grades the SDR range, and the
/// light above SDR white passes through it rather than clipping there.
#[test]
fn an_hdr_delivery_renders_display_light() {
    use klipp_color::hdr::{pq_decode_nits, pq_encode_nits, HdrTransfer, HDR_REFERENCE_WHITE_NITS};
    let hdr = RenderOptions { hdr_peak_nits: Some(1000.0), ..Default::default() };
    let render_hdr = |seq: &Sequence, src: &mut Flat, lut: Option<&klipp_color::lut::LutData>| {
        [false, true].map(|gpu| {
            let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            r.set_gpu_enabled(gpu);
            r.set_global_lut(lut.map(|l| klipp_compose::GlobalLut { lut: l.clone(), opacity: 1.0 }));
            r.render(seq, tc(1.0), W, H, src, hdr)
        })
    };
    let code = (pq_encode_nits(1000.0) * 1023.0).round() as u32;
    let pq_white = pq_decode_nits(code as f32 / 1023.0) / HDR_REFERENCE_WHITE_NITS;
    let white = solid([255, 255, 255], 0.0, 2.0);
    let (pq, log) = (media(0.0, 2.0), media(0.0, 2.0));
    for (label, c, src, want) in [
        ("SDR white", white.clone(), Flat::default(), 1.0),
        ("PQ 1000 nits", pq.clone(), Flat::default().scene(&pq, HdrTransfer::Pq, code), pq_white),
        ("S-Log3 grey", log.clone(), Flat::default().slog3(&log, 420), 26.0 / HDR_REFERENCE_WHITE_NITS),
    ] {
        let mut src = src;
        for (i, img) in render_hdr(&sequence(vec![vec![c]]), &mut src, None).iter().enumerate() {
            assert_px(&format!("{label}, run {i}"), px(img, 16, 9), [want; 3], want * 2e-3);
        }
    }
    // A LUT that darkens everything to 80% of its encoded value.
    let n = 5usize;
    let mut data = Vec::new();
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                data.extend([r, g, b].map(|v| 0.8 * v as f32 / (n - 1) as f32));
            }
        }
    }
    let lut = klipp_color::lut::LutData {
        size: n, data, input_space: Default::default(), output_space: Default::default(),
    };
    let darkened_white = klipp_color::transfer::srgb_to_linear(0.8);
    let mut src = Flat::default().scene(&pq, HdrTransfer::Pq, code);
    for (i, img) in render_hdr(&sequence(vec![vec![pq]]), &mut src, Some(&lut)).iter().enumerate() {
        let want = darkened_white + (pq_white - 1.0);
        assert_px(&format!("PQ through the LUT, run {i}"), px(img, 16, 9), [want; 3], 5e-3);
    }
    for (i, img) in render_hdr(&sequence(vec![vec![white]]), &mut Flat::default(), Some(&lut)).iter().enumerate() {
        assert_px(&format!("SDR white through the LUT, run {i}"), px(img, 16, 9), [darkened_white; 3], 5e-3);
    }
}

/// Temperature and tint are a white balance: a chromatic adaptation, so a
/// grey four times brighter is balanced exactly as the dim one, black stays
/// black, and it is the same on the CPU and the GPU, in the Corrector and
/// the Color Balance node. They used to add a fixed amount, which tinted
/// blacks and did next to nothing to scene-linear highlights.
///
/// It is a matrix, not a per-channel gain — at 40/-15 the green term
/// contributes 0.113 to red — so the grey cases below can only see its
/// *row sums*. The coloured cases see its columns, and
/// `a_grey_cannot_see_the_white_balance_columns` states why they have to
/// exist.
#[test]
fn temperature_and_tint_balance_every_exposure_alike() {
    for kind in [NodeKind::Corrector, NodeKind::ColorBalance] {
        let p = NodeParams {
            temperature: Some(40.0),
            tint: Some(-15.0),
            ..Default::default()
        };
        let want = klipp_color::white_balance::white_balance_matrix(40.0, -15.0, Default::default()).unwrap();
        for grey in [0.0f32, 0.18, 0.72, 6.0] {
            let mut c = media(0.0, 2.0);
            c.node_graph = graph(kind, p.clone(), Default::default());
            let mut src = Flat::default().grey(&c, grey);
            let expect = want.map(|row| (row[0] + row[1] + row[2]) * grey);
            for (i, img) in render(&sequence(vec![vec![c]]), 1.0, &mut src).iter().enumerate() {
                let tol = 1e-4 + grey * 1e-4;
                assert_px(&format!("{kind:?}, grey {grey}, run {i}"), px(img, 16, 9), expect, tol);
            }
        }

        // Colour, so the matrix's columns are observable. Each of these
        // leans hard on one input channel, which is what makes a
        // cross-channel mix-up — the red term applied to green, say —
        // show up; on a grey it cannot. Kept off the pure primaries
        // because two of the columns carry small negatives and a pure
        // primary would drive the result under zero, which is a different
        // argument than the one this test is making.
        for rgb in [[0.8f32, 0.1, 0.1], [0.1, 0.8, 0.1], [0.1, 0.1, 0.8]] {
            let mut c = media(0.0, 2.0);
            c.node_graph = graph(kind, p.clone(), Default::default());
            let mut src = Flat::default().linear(&c, rgb);
            // The real matrix multiply, not a row sum.
            let expect = want.map(|row| row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2]);
            for (i, img) in render(&sequence(vec![vec![c]]), 1.0, &mut src).iter().enumerate() {
                assert_px(
                    &format!("{kind:?}, colour {rgb:?}, run {i}"),
                    px(img, 16, 9),
                    expect,
                    2e-4,
                );
            }
        }
    }
}

/// Why the test above needs its coloured cases, stated as an assertion
/// rather than left in a comment.
///
/// Permuting a 3x3's columns permutes which input channel each term
/// applies to — the shape of a cross-channel mix-up. It leaves every row
/// *sum* untouched, so a grey sees nothing: the two matrices balance a
/// grey identically to within float summation order, far inside the
/// tolerance that test uses. A colour sees it immediately.
///
/// If this ever starts failing because the matrix went diagonal, the
/// coloured cases above stop being load-bearing and can go.
#[test]
fn a_grey_cannot_see_the_white_balance_columns() {
    let m = klipp_color::white_balance::white_balance_matrix(40.0, -15.0, Default::default()).unwrap();
    let permuted = m.map(|r| [r[2], r[0], r[1]]);
    let apply = |x: [[f32; 3]; 3], v: [f32; 3]| {
        x.map(|row| row[0] * v[0] + row[1] * v[1] + row[2] * v[2])
    };

    // The matrix has to actually mix channels, or there is nothing here.
    let off_diagonal = (m[0][1].abs() + m[0][2].abs() + m[1][0].abs()
        + m[1][2].abs() + m[2][0].abs() + m[2][1].abs())
        / 6.0;
    assert!(
        off_diagonal > 0.01,
        "the white balance is nearly diagonal ({off_diagonal:.4}); the coloured cases are moot",
    );

    // Grey: blind. Both matrices land within the grey cases' tolerance.
    let grey = [0.18f32; 3];
    let (a, b) = (apply(m, grey), apply(permuted, grey));
    let grey_gap = (0..3).map(|i| (a[i] - b[i]).abs()).fold(0.0f32, f32::max);
    assert!(
        grey_gap < 1e-4,
        "a grey distinguishes the permutation by {grey_gap:e}, so it is not blind after all",
    );

    // Colour: not blind. Orders of magnitude outside the same tolerance.
    let colour = [0.8f32, 0.1, 0.1];
    let (a, b) = (apply(m, colour), apply(permuted, colour));
    let colour_gap = (0..3).map(|i| (a[i] - b[i]).abs()).fold(0.0f32, f32::max);
    assert!(
        colour_gap > 0.1,
        "a colour only distinguishes the permutation by {colour_gap:e}; pick a stronger fixture",
    );
    eprintln!("grey sees {grey_gap:e}, colour sees {colour_gap:e} — ratio {:.0}x", colour_gap / grey_gap.max(1e-12));
}

/// A colour the user authored means the same colour in every working
/// space. Titles, generators and an effect's colour parameters are
/// written in BT.709; they used to land in the working space unconverted,
/// so a "pure red" title in a BT.2020 or ACEScg project was that space's
/// red — more saturated than the one picked — and a chroma key missed the
/// green it was pointed at.
#[test]
fn an_authored_colour_is_the_same_colour_in_any_working_space() {
    use klipp_core::WorkingColorspace::*;
    let red = solid([255, 0, 0], 0.0, 2.0);
    let seq = sequence(vec![vec![red]]);
    let in_space = |ws| {
        [false, true].map(|gpu| {
            let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            r.set_gpu_enabled(gpu);
            r.set_working_colorspace(ws);
            let img = r.render(&seq, tc(1.0), W, H, &mut Flat::default(), RenderOptions::default());
            px(&img, 16, 9)
        })
    };
    let in_709 = in_space(Rec709Linear);
    for (ws, name) in [(Bt2020Linear, "BT.2020"), (AcesCG, "ACEScg")] {
        let converted = klipp_color::primaries::working_space_conversion_matrix(Rec709Linear, ws).unwrap();
        let want = klipp_color::primaries::matrix3_mul_vec(converted, in_709[0]);
        for (i, got) in in_space(ws).iter().enumerate() {
            assert_px(&format!("red in a {name} project, run {i}"), *got, want, 2e-3);
        }
    }
    // And the same colour in the other direction: a BT.2020 project's red
    // is not BT.2020's own red, which would be more saturated than asked.
    let wide = in_space(Bt2020Linear)[0];
    assert!(wide[1] > 0.02 && wide[2] > 0.005, "red in a BT.2020 project came back as BT.2020 red: {wide:?}");
}

/// An effect's colour parameter follows the same rule: a chroma key keys
/// the green it was given, whatever the working space.
#[test]
fn a_chroma_key_keys_its_colour_in_any_working_space() {
    use klipp_core::WorkingColorspace::*;
    let mut green = solid([0, 255, 0], 0.0, 2.0);
    green.builtin_effects = vec![BuiltinEffect::ChromaKey {
        key_color: [0, 255, 0],
        tolerance: 0.15,
        soft_edge: 0.05,
        spill_method: klipp_effects::SpillMethod::None,
        spill_strength: 0.0,
    }];
    let seq = sequence(vec![vec![green]]);
    for (ws, name) in [(Rec709Linear, "BT.709"), (Bt2020Linear, "BT.2020"), (AcesCG, "ACEScg")] {
        for gpu in [false, true] {
            let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            r.set_gpu_enabled(gpu);
            r.set_working_colorspace(ws);
            let opts = RenderOptions {
                keep_alpha: true,
                ..Default::default()
            };
            let img = r.render(&seq, tc(1.0), W, H, &mut Flat::default(), opts);
            let alpha = img[((9 * W + 16) * 4 + 3) as usize];
            assert!(alpha < 0.1, "{name} (gpu {gpu}): the key left alpha at {alpha}");
        }
    }
}

/// Node keyframes are read at clip-relative time, where the UI writes
/// them. Exposure ramps 0 → 2 stops over the clip's first two seconds;
/// one second in it is one stop, even though the clip's source starts
/// ten seconds into its media. At source time it would read two stops.
#[test]
fn node_keyframes_use_clip_relative_time() {
    let mut kf = klipp_nodes::NodeKeyframeTracks::default();
    let mut keys = Vec::new();
    klipp_core::keyframe::upsert_keyframe(&mut keys, 0.0, 0.0);
    klipp_core::keyframe::upsert_keyframe(&mut keys, 2.0, 2.0);
    kf.tracks.push(klipp_nodes::NodeKeyframeTrack { param_name: "exposure".into(), keyframes: keys });
    let p = NodeParams {
        exposure: Some(0.0),
        ..Default::default()
    };
    let mut c = media(0.0, 4.0);
    c.source_in = tc(10.0);
    c.source_out = tc(14.0);
    c.node_graph = graph(NodeKind::Exposure, p, kf);
    let mut src = Flat::default().grey(&c, 0.1);
    for (i, img) in render(&sequence(vec![vec![c]]), 1.0, &mut src).iter().enumerate() {
        assert_px(&format!("run {i}"), px(img, 16, 9), [0.2; 3], 0.005);
    }
}

/// Generated colours arrive as their sRGB values.
#[test]
fn generators_render_their_colour() {
    let seq = sequence(vec![vec![solid([200, 40, 40], 0.0, 2.0)]]);
    for (i, img) in render(&seq, 1.0, &mut Flat::default()).iter().enumerate() {
        assert_px(&format!("run {i}"), px(img, 16, 9), [srgb_linear(200), srgb_linear(40), srgb_linear(40)], 1e-3);
    }
}

/// Source time runs through the clip's speed, into its handles either
/// side for transitions.
#[test]
fn source_time_follows_speed_into_the_handles() {
    let mut c = media(4.0, 8.0);
    c.source_in = tc(5.0);
    c.source_out = tc(13.0);
    c.speed = 2.0;
    let s = |t| klipp_compose::render::source_secs(&c, tc(t), 24.0);
    assert!((s(4.0) - 5.0).abs() < 1e-6);
    assert!((s(5.0) - 7.0).abs() < 0.05);
    assert!((s(3.5) - 4.0).abs() < 1e-6, "before the clip: {}", s(3.5));
    assert!((s(8.5) - 14.0).abs() < 0.1, "after the clip: {}", s(8.5));
}

fn luma_stencil() -> klipp_timeline::StencilSettings {
    klipp_timeline::StencilSettings {
        channel: klipp_nodes::params::MatteChannel::Luminance, invert: false, feather_px: 0.0, choke_px: 0.0,
    }
}

/// A stencil masks every layer *below* it and nothing above, and is not
/// itself shown. Black stencil (luminance 0) over green: the green is cut
/// out; a half-opacity red above still shows over the black background.
/// (The viewer's "above" test read track indices the wrong way round.)
#[test]
fn a_stencil_masks_only_what_is_below_it() {
    let mut stencil = solid([0, 0, 0], 0.0, 2.0);
    stencil.matte_stencil = Some(luma_stencil());
    let mut seq = sequence(vec![
        vec![solid([255, 0, 0], 0.0, 2.0)], // above the stencil
        vec![stencil],
        vec![solid([0, 255, 0], 0.0, 2.0)], // below it
    ]);
    seq.tracks[0].opacity = 0.5;
    for (i, img) in render(&seq, 1.0, &mut Flat::default()).iter().enumerate() {
        assert_px(&format!("run {i}"), px(img, 16, 9), [0.5, 0.0, 0.0], 1e-3);
    }
}

/// A track matte masks its consumer by the source clip's rendered pixels;
/// a source consumed as a matte is not shown itself.
#[test]
fn a_track_matte_masks_its_consumer() {
    for (src_rgb, want) in [([255, 255, 255], [0.0, 1.0, 0.0]), ([0, 0, 0], [0.0, 0.0, 0.0])] {
        let mut source = solid(src_rgb, 0.0, 2.0);
        source.consumed_as_matte = true;
        let mut consumer = solid([0, 255, 0], 0.0, 2.0);
        let mut binding = klipp_timeline::MatteBinding::new(source.id);
        binding.channel = klipp_nodes::params::MatteChannel::Luminance;
        consumer.matte_source = Some(binding);
        let seq = sequence(vec![vec![source], vec![consumer]]);
        for (i, img) in render(&seq, 1.0, &mut Flat::default()).iter().enumerate() {
            assert_px(&format!("source {src_rgb:?}, run {i}"), px(img, 16, 9), want, 1e-3);
        }
    }
}

/// An adjustment layer grades everything beneath it, mixed by its opacity.
#[test]
fn an_adjustment_layer_grades_what_is_below_it() {
    let base = media(0.0, 2.0);
    let mut src = Flat::default().grey(&base, 0.2);
    let mut adj = clip(ClipKind::AdjustmentLayer, 0.0, 2.0);
    adj.node_graph = exposure(1.0);
    let mut seq = sequence(vec![vec![adj], vec![base]]);
    for (i, img) in render(&seq, 1.0, &mut src).iter().enumerate() {
        assert_px(&format!("full, run {i}"), px(img, 16, 9), [0.4; 3], 0.005);
    }
    seq.tracks[0].opacity = 0.5;
    for (i, img) in render(&seq, 1.0, &mut src).iter().enumerate() {
        assert_px(&format!("half, run {i}"), px(img, 16, 9), [0.3; 3], 0.005);
    }
}

/// A multicam group shows the angle its primary clip's cut list chooses:
/// angle 0 until the first cut, then the cut's angle. (Only export used
/// to honour cut lists; the viewer showed whichever angle came first.)
#[test]
fn multicam_shows_the_cut_angle() {
    let group = klipp_core::GroupId::new();
    let mut primary = media(0.0, 4.0);
    primary.multicam_group = Some(group);
    primary.multicam_cuts = vec![(2.0, 1)];
    let mut angle1 = media(0.0, 4.0);
    angle1.multicam_group = Some(group);
    angle1.multicam_angle = 1;
    let mut src = Flat::default().grey(&primary, 0.2).grey(&angle1, 0.6);
    // Angle 1 listed first, so "first clip under the playhead" would pick it.
    let seq = sequence(vec![vec![angle1, primary]]);
    for (t, want) in [(1.0, 0.2), (3.0, 0.6)] {
        for (i, img) in render(&seq, t, &mut src).iter().enumerate() {
            assert_px(&format!("t={t}, run {i}"), px(img, 16, 9), [want; 3], 1e-3);
        }
    }
}

/// Two frames of a clip's media, at 0 and 1/24 s, for frame blending.
struct TwoFrames {
    clip: ClipId,
    first: f32,
    second: f32,
}

impl TwoFrames {
    fn at(v: f32, pts: f64) -> SourceFrame {
        SourceFrame {
            pixels: SourcePixels::Linear(Arc::new([v, v, v, 1.0].repeat((W * H) as usize))),
            width: W,
            height: H,
            pts: Some(pts),
        }
    }
}

impl FrameSource for TwoFrames {
    fn frame(&mut self, clip: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        (clip.id == self.clip).then(|| Self::at(self.first, 0.0))
    }
    fn frame_after(&mut self, clip: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        (clip.id == self.clip).then(|| Self::at(self.second, 1.0 / 24.0))
    }
}

/// Slow motion with frame blending mixes the source frames either side
/// of the source time, by where it falls between them. At half speed, one
/// timeline frame in is half a source frame in: half of each. Without
/// the blend mode, the earlier frame alone.
#[test]
fn slow_motion_blends_the_frames_either_side() {
    let ramp = |blend_mode| BuiltinEffect::SpeedRamp {
        speed: 0.5, blend_mode, ramp_curve: 0, ramp_start: 0.0, ramp_end: 1.0, reverse: false, freeze_at: 0.0, freeze: false,
    };
    for blend_mode in [1, 0] {
        let mut c = media(0.0, 4.0);
        c.speed = 0.5;
        c.builtin_effects = vec![ramp(blend_mode)];
        // Where the source time falls between the frames, through the
        // clip's speed: about half.
        let weight = klipp_compose::render::source_secs(&c, tc(1.0 / 24.0), 24.0) as f32 * 24.0;
        assert!((weight - 0.5).abs() < 0.02, "source time weight {weight}");
        let want = if blend_mode == 1 { 0.2 + 0.4 * weight } else { 0.2 };
        let seq = sequence(vec![vec![c.clone()]]);
        for gpu in [false, true] {
            let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            r.set_gpu_enabled(gpu);
            let mut src = TwoFrames { clip: c.id, first: 0.2, second: 0.6 };
            let img = r.render(&seq, tc(1.0 / 24.0), W, H, &mut src, RenderOptions::default());
            assert_px(&format!("blend mode {blend_mode}, gpu {gpu}"), px(&img, 16, 9), [want; 3], 1e-3);
        }
    }
}

/// Inspecting a clip returns its picture before the grade, alongside the
/// graded composite, and changes nothing about the picture.
#[test]
fn inspecting_a_clip_returns_its_ungraded_picture() {
    let mut c = media(0.0, 2.0);
    c.node_graph = exposure(1.0);
    let seq = sequence(vec![vec![c.clone()]]);
    let mut src = Flat::default().grey(&c, 0.2);
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    let inspect = Inspect { clip: Some(c.id), ungraded: true, ..Default::default() };
    let (img, found) = r.render_inspected(&seq, tc(1.0), W, H, &mut src, RenderOptions::default(), inspect);
    assert_px("graded", px(&img, 16, 9), [0.4; 3], 0.005);
    let before = found.ungraded.expect("the ungraded picture");
    assert_px("before", px(&before, 16, 9), [0.2; 3], 1e-4);
    let plain = r.render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default());
    assert_eq!(img, plain);
}

/// `active_layers` names what a render stacks: a transition's two clips
/// on its track, a clip above it, bottom first.
#[test]
fn active_layers_lists_what_a_render_needs() {
    let a = media(0.0, 2.0);
    let b = media(2.0, 4.0);
    let top = solid([255, 0, 0], 0.0, 4.0);
    let tr = Transition {
        id: TransitionId::new(),
        kind: klipp_core::TransitionKind::CrossDissolve,
        duration: tc(1.0),
        clip_a_id: a.id,
        clip_b_id: b.id,
        user_params: Default::default(),
        alignment: klipp_core::TransitionAlignment::Center,
    };
    let (a_id, b_id, top_id) = (a.id, b.id, top.id);
    let mut seq = sequence(vec![vec![top], vec![a, b]]);
    seq.tracks[1].transitions.push(tr);
    let layers = active_layers(&seq, tc(2.0));
    assert_eq!(layers.len(), 2);
    assert_eq!(layers[0].track, 1);
    assert!(layers[0].transition);
    assert_eq!(layers[0].clips.iter().map(|c| c.id).collect::<Vec<_>>(), vec![a_id, b_id]);
    assert_eq!(layers[1].clips[0].id, top_id);
    let later = active_layers(&seq, tc(3.0));
    assert_eq!(later[0].clips.iter().map(|c| c.id).collect::<Vec<_>>(), vec![b_id]);
    assert!(!later[0].transition);
}

/// A frame snapshot renders exactly what the whole sequence does: through
/// a transition, before and after its cut, and with a clip whose
/// transform follows another that is not on screen.
#[test]
fn a_frame_snapshot_renders_the_same_frame() {
    let a = solid([255, 0, 0], 0.0, 2.0);
    let mut b = media(2.0, 4.0);
    b.node_graph = exposure(1.0);
    let far = solid([0, 0, 255], 10.0, 12.0);
    let mut follower = solid([0, 255, 0], 0.0, 4.0);
    follower.builtin_effects = vec![BuiltinEffect::Transform {
        position_x: 0.0, position_y: 0.0, scale_x: 0.5, scale_y: 0.5, rotation: 0.0,
        anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
    }];
    follower.transform_link = Some(klipp_timeline::TransformLink::new(far.id));
    let tr = Transition {
        id: TransitionId::new(),
        kind: klipp_core::TransitionKind::CrossDissolve,
        duration: tc(1.0),
        clip_a_id: a.id,
        clip_b_id: b.id,
        user_params: Default::default(),
        alignment: klipp_core::TransitionAlignment::Center,
    };
    let mut src = Flat::default().grey(&b, 0.25);
    let mut seq = sequence(vec![vec![follower, far], vec![a, b]]);
    seq.tracks[1].transitions.push(tr);
    for t in [1.0, 1.75, 2.25, 3.0] {
        let snap = klipp_compose::frame_snapshot(&seq, tc(t));
        let full = render(&seq, t, &mut src);
        let part = render(&snap, t, &mut src);
        assert_eq!(full[0], part[0], "CPU, t={t}");
        assert_eq!(full[1], part[1], "GPU, t={t}");
    }
}

/// Transitions other than the dissolve render the same with and without
/// a GPU: the CPU runs the transition shader transcribed. (Its fallback
/// mixed 8-bit sRGB bytes with a separate implementation of each kind.)
#[test]
fn every_transition_renders_the_same_with_and_without_a_gpu() {
    for kind in [klipp_core::TransitionKind::WipeLeft, klipp_core::TransitionKind::FilmDissolve, klipp_core::TransitionKind::DipToBlack] {
        let a = solid([230, 40, 40], 0.0, 2.0);
        let mut b = media(2.0, 4.0);
        b.node_graph = exposure(1.0);
        let mut src = Flat::default().grey(&b, 0.2);
        let tr = Transition {
            id: TransitionId::new(),
            kind,
            duration: tc(1.0),
            clip_a_id: a.id,
            clip_b_id: b.id,
            user_params: Default::default(),
            alignment: klipp_core::TransitionAlignment::Center,
        };
        let mut seq = sequence(vec![vec![a, b]]);
        seq.tracks[0].transitions.push(tr);
        let [cpu, gpu] = render(&seq, 2.1, &mut src);
        let worst = cpu.iter().zip(&gpu).map(|(c, g)| (c - g).abs()).fold(0.0f32, f32::max);
        assert!(worst < 2e-3, "{kind:?}: CPU and GPU differ by {worst}");
    }
}

/// A default title on a text track above a solid shows up in the
/// composite: the layer model composites titles like any other layer.
#[test]
fn a_title_on_a_text_track_shows_over_the_video() {
    let mut seq = Sequence::new_default();
    let mut text = Track::new("Text 1", TrackKind::Text);
    let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { font_size: 240.0, ..Default::default() });
    text.clips = vec![clip(ClipKind::Title(td), 0.3, 5.3)];
    let mut video = Track::new("V1", TrackKind::Video);
    video.clips = vec![solid([40, 40, 40], 0.0, 10.0)];
    seq.tracks = vec![text, video];
    let mut src = Flat(HashMap::new());
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    let img = r.render(&seq, tc(2.0), 320, 180, &mut src, RenderOptions::default());
    let bright = img.chunks_exact(4).filter(|p| p[0] > 0.5).count();
    assert!(bright > 400, "the title's white glyphs should be in the frame, bright pixels: {bright}");
}

/// The same over a media clip, on both paths: the engine's frame is
/// a media clip, and its path runs with the GPU on.
#[test]
fn a_title_on_a_text_track_shows_over_a_media_clip_on_both_paths() {
    let mut seq = Sequence::new_default();
    let mut text = Track::new("Text 1", TrackKind::Text);
    let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { font_size: 240.0, ..Default::default() });
    text.clips = vec![clip(ClipKind::Title(td), 0.3, 5.3)];
    let mut video = Track::new("V1", TrackKind::Video);
    let m = media(0.0, 10.0);
    let mut src = Flat::default();
    src.0.insert(m.id, Box::new(|| SourcePixels::Linear(Arc::new([0.02, 0.02, 0.02, 1.0].repeat((320 * 180) as usize)))));
    video.clips = vec![m];
    seq.tracks = vec![text, video];
    for gpu in [false, true] {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(gpu);
        let img = r.render(&seq, tc(2.0), 320, 180, &mut src, RenderOptions::default());
        let bright = img.chunks_exact(4).filter(|p| p[0] > 0.5).count();
        assert!(bright > 400, "gpu={gpu}: the title should be over the media clip, bright pixels: {bright}");
    }
}

/// The live shape that showed nothing: a portrait UHD sequence, the
/// default 72 px title on a text track above two video tracks that
/// carry the same media, GPU on.
#[test]
fn a_default_title_shows_on_a_portrait_uhd_stack() {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: 2160, height: 3840 };
    let mut text = Track::new("Text 1", TrackKind::Text);
    text.clips = vec![clip(ClipKind::Title(klipp_timeline::TitleData::default()), 0.3, 5.3)];
    let mid = klipp_core::MediaId::new();
    let mut v1 = Track::new("V1", TrackKind::Video);
    let mut v2 = Track::new("V2", TrackKind::Video);
    let a = clip(ClipKind::Media { media_id: mid }, 0.0, 48.0);
    let b = clip(ClipKind::Media { media_id: mid }, 0.0, 48.0);
    let mut src = Flat::default();
    for c in [&a, &b] {
        src.0.insert(c.id, Box::new(|| SourcePixels::Linear(Arc::new([0.02, 0.02, 0.02, 1.0].repeat((2160 * 3840) as usize)))));
    }
    v1.clips = vec![a];
    v2.clips = vec![b];
    seq.tracks = vec![text, v1, v2];
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let img = r.render(&seq, tc(2.0), 2160, 3840, &mut src, RenderOptions::default());
    let bright = img.chunks_exact(4).filter(|p| p[0] > 0.5).count();
    assert!(bright > 5000, "bright pixels: {bright}");
}

/// A title's motion blur used to place one rasterisation at every shutter
/// sample, so a title whose own drawing moves was averaged to itself. A
/// word rising into place came out as sharp letters at one position —
/// the sharpness motion blur exists to remove. The letters are what move
/// here; the clip never does.
///
/// The title renderer scales type by height/1080, so the frame has to be
/// big enough and the size large enough for real glyphs. The animation
/// is always the same; only what is asked of the renderer differs.
fn entering_title(enter: klipp_core::title_motion::Motion, motion_blur: Option<f32>, color: [u8; 4]) -> (Sequence, Clip) {
    let tb = klipp_timeline::TextBox { text: "RISE".into(), font_size: 420.0, color, ..Default::default() };
    let fx = klipp_timeline::LayerFx {
        motion: klipp_timeline::TitleMotion::enter(enter),
        motion_blur: motion_blur.map(|shutter_angle| klipp_timeline::MotionBlur {
            enabled: true,
            shutter_angle,
            shutter_angle_keyframes: Vec::new(),
            samples: 8,
        }),
        ..Default::default()
    };
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: 480, height: 270 };
    let title = clip(ClipKind::Title(klipp_timeline::TitleData::single_text_fx(tb, fx)), 0.0, 5.0);
    let mut text = Track::new("Text 1", TrackKind::Text);
    text.clips = vec![title.clone()];
    let mut video = Track::new("V1", TrackKind::Video);
    video.clips = vec![media(0.0, 5.0)];
    seq.tracks = vec![text, video];
    (seq, title)
}

/// Each letter rising over a tenth of a second, one after the next: at
/// 0.25 s some are in place and some are still travelling, which is the
/// moment worth sampling.
fn rising_letters() -> klipp_core::title_motion::Motion {
    klipp_core::title_motion::Motion {
        kind: klipp_core::title_motion::MotionKind::Rise,
        duration_secs: 0.1,
        delay_secs: 0.0,
        easing: Default::default(),
        stagger: klipp_core::title_motion::StaggerUnit::Char,
        stagger_secs: 0.08,
        distance: 1.0,
    }
}

/// The frame at 0.25 s of a sequence from [`entering_title`], on the CPU
/// path, over a white source.
fn entering_title_frame(seq: &Sequence, prepare: impl FnOnce(&mut Renderer)) -> Vec<f32> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    prepare(&mut r);
    r.render(seq, tc(0.25), 480, 270, &mut White, RenderOptions::default())
}

/// Partly-covered pixels: a sharp glyph edge makes few, a smeared one
/// makes many.
fn soft(img: &[f32]) -> usize {
    img.chunks_exact(4).filter(|p| (0.05..0.95).contains(&p[0])).count()
}

#[test]
fn a_stationary_title_whose_letters_rise_is_smeared_by_them() {
    // Dark on the white source, so a glyph's coverage reads as a value
    // between the two.
    let shot = |shutter_angle: f32| entering_title_frame(&entering_title(rising_letters(), Some(shutter_angle), [0, 0, 0, 255]).0, |_| {});
    let (closed, open) = (shot(0.0), shot(360.0));
    assert!(
        soft(&closed) > 0,
        "the title should be on screen at all: no partly-covered pixels with the shutter closed",
    );
    assert!(
        soft(&open) * 2 > soft(&closed) * 3,
        "a letter travelling while the shutter is open should smear: {} partly-covered pixels against {} with it closed (measured 5317 against 2737)",
        soft(&open),
        soft(&closed),
    );
}

/// A shutter that never opens changes nothing: every sample is the frame
/// itself, and a title drawn once a sample has to come out as the title
/// drawn once a frame. The three that follow pin each thing a sample was
/// once drawn without — the grade, the working space, the blur it enters
/// with — by comparing the shut shutter to no motion blur at all.
fn assert_shut_shutter_is_identity(what: &str, off: &[f32], shut: &[f32]) {
    assert!(soft(off) > 0, "the title should be on screen at all for the {what} comparison to mean anything");
    let worst = off.iter().zip(shut).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(
        worst < 1e-4,
        "a shut shutter should draw exactly what no motion blur draws, but the {what} differs by up to {worst}",
    );
}

#[test]
fn a_shut_shutter_keeps_the_title_graded() {
    // Grey, so a stop of exposure visibly lifts it.
    let shot = |mb| {
        let (mut seq, title) = entering_title(rising_letters(), mb, [110, 110, 110, 255]);
        let graded = Clip { node_graph: exposure(1.0), ..title };
        seq.tracks[0].clips = vec![graded];
        entering_title_frame(&seq, |_| {})
    };
    let ungraded = entering_title_frame(&entering_title(rising_letters(), None, [110, 110, 110, 255]).0, |_| {});
    let off = shot(None);
    assert!(
        off.iter().zip(&ungraded).any(|(a, b)| (a - b).abs() > 0.05),
        "the grade should show on the letters at all, or the comparison proves nothing",
    );
    assert_shut_shutter_is_identity("grade", &off, &shot(Some(0.0)));
}

#[test]
fn a_shut_shutter_keeps_the_title_in_the_working_space() {
    use klipp_core::WorkingColorspace::AcesCG;
    // A saturated colour: read as ACEScg primaries by mistake, it is a
    // different colour; brought across properly, the same one.
    let shot = |mb| entering_title_frame(&entering_title(rising_letters(), mb, [220, 40, 40, 255]).0, |r| r.set_working_colorspace(AcesCG));
    assert_shut_shutter_is_identity("colour in an ACEScg project", &shot(None), &shot(Some(0.0)));
}

#[test]
fn a_shut_shutter_keeps_the_blur_a_title_enters_with() {
    // A blur entrance is not posed a letter at a time: the whole title
    // is rastered and the blur deferred to the compositor, which is the
    // step a sample can miss. So the title enters as one, slowly enough
    // to still be blurred at 0.25 s.
    let blurred_entrance = klipp_core::title_motion::Motion {
        kind: klipp_core::title_motion::MotionKind::Blur,
        duration_secs: 0.4,
        stagger: klipp_core::title_motion::StaggerUnit::None,
        ..rising_letters()
    };
    let shot = |mb| entering_title_frame(&entering_title(blurred_entrance, mb, [0, 0, 0, 255]).0, |_| {});
    assert_shut_shutter_is_identity("blur entrance", &shot(None), &shot(Some(0.0)));
}

/// A precomposition is its scene composed first and treated as one
/// picture after. Here the scene is green under half-transparent red —
/// (0.5, 0.5, 0) composed — and the precomposition is faded to half:
/// the parent's opacity lands once, on the composed picture. Flattened
/// as a compound, each child would be faded on its own over the parent's
/// background and the red would read against black rather than green.
#[test]
fn a_precomposition_is_composed_before_the_parent_fades_it() {
    let mut red = solid([255, 0, 0], 0.0, 4.0);
    klipp_core::keyframe::upsert_keyframe(&mut red.opacity_keyframes, 0.0, 0.5);
    let scene = sequence(vec![vec![red], vec![solid([0, 255, 0], 0.0, 4.0)]]);
    let mut pre = Clip::precomposition(scene.id, tc(0.0), tc(4.0));
    klipp_core::keyframe::upsert_keyframe(&mut pre.opacity_keyframes, 0.0, 0.5);
    let outer = sequence(vec![vec![pre]]);
    let flat = klipp_timeline::flatten::flatten_sequence(&outer, &[outer.clone(), scene.clone()]);
    for (i, img) in render(&flat, 1.0, &mut Flat::default()).iter().enumerate() {
        assert_px(&format!("the composed scene at half, run {i}"), px(img, 16, 9), [0.25, 0.25, 0.0], 2e-3);
    }
}

/// The parent's transform moves the composed picture, once: scaled to
/// half, the scene is a half-size picture of itself in the middle of
/// the frame with nothing around it.
#[test]
fn a_precomposition_is_transformed_as_one_picture() {
    let mut red = solid([255, 0, 0], 0.0, 4.0);
    klipp_core::keyframe::upsert_keyframe(&mut red.opacity_keyframes, 0.0, 0.5);
    let scene = sequence(vec![vec![red], vec![solid([0, 255, 0], 0.0, 4.0)]]);
    let mut pre = Clip::precomposition(scene.id, tc(0.0), tc(4.0));
    pre.builtin_effects = vec![BuiltinEffect::Transform {
        position_x: 0.0, position_y: 0.0, scale_x: 0.5, scale_y: 0.5, rotation: 0.0,
        anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
    }];
    let outer = sequence(vec![vec![pre]]);
    let flat = klipp_timeline::flatten::flatten_sequence(&outer, &[outer.clone(), scene.clone()]);
    for (i, img) in render(&flat, 1.0, &mut Flat::default()).iter().enumerate() {
        assert_px(&format!("the scene, in the middle, run {i}"), px(img, 16, 9), [0.5, 0.5, 0.0], 2e-3);
        assert_px(&format!("nothing at the corner, run {i}"), px(img, 1, 1), [0.0, 0.0, 0.0], 1e-3);
        let lit = img.chunks_exact(4).filter(|p| p[1] > 0.1).count();
        let quarter = (W * H / 4) as usize;
        assert!((lit as i64 - quarter as i64).unsigned_abs() as usize <= quarter / 6, "a quarter of the frame is scene: {lit} of {}", W * H);
    }
}

/// A scene's camera looks at the scene: it magnifies what is inside the
/// precomposition and nothing outside it. A small card that its scene's
/// camera zooms in on twice fills four times the area it would.
#[test]
fn a_scenes_camera_looks_at_the_scene_not_the_parent() {
    let card = || {
        let mut c = solid([0, 255, 0], 0.0, 4.0);
        c.builtin_effects = vec![BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0, scale_x: 0.25, scale_y: 0.25, rotation: 0.0,
            anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
        }];
        c
    };
    let lit_area = |zoom: Option<f32>| {
        let mut scene = sequence(vec![vec![card()]]);
        if let Some(z) = zoom {
            klipp_core::keyframe::upsert_keyframe(&mut scene.camera.zoom_keyframes, 0.0, z);
        }
        let outer = sequence(vec![vec![Clip::precomposition(scene.id, tc(0.0), tc(4.0))]]);
        let flat = klipp_timeline::flatten::flatten_sequence(&outer, &[outer.clone(), scene.clone()]);
        render(&flat, 1.0, &mut Flat::default()).map(|img| img.chunks_exact(4).filter(|p| p[1] > 0.1).count())
    };
    let (plain, zoomed) = (lit_area(None), lit_area(Some(2.0)));
    for i in 0..2 {
        assert!(plain[i] > 0, "the card is on screen at all, run {i}");
        let want = plain[i] * 4;
        assert!((zoomed[i] as i64 - want as i64).unsigned_abs() as usize <= want / 5, "zoomed twice in the scene, the card covers four times the area: {} against {}, run {i}", zoomed[i], plain[i]);
    }
}

/// The same scene laid in twice at once, offset in time, is two sets of
/// clips to a frame source: two ids, two source times. With one set, a
/// single decoder would serve both instances the same frame.
#[test]
fn a_scene_laid_in_twice_asks_for_its_media_twice() {
    let scene = sequence(vec![vec![media(0.0, 6.0)]]);
    let first = Clip::precomposition(scene.id, tc(0.0), tc(4.0));
    let mut second = Clip::precomposition(scene.id, tc(0.0), tc(4.0));
    second.source_in = tc(1.0);
    second.source_out = tc(5.0);
    let outer = sequence(vec![vec![first], vec![second]]);
    let flat = klipp_timeline::flatten::flatten_sequence(&outer, &[outer.clone(), scene.clone()]);
    let layers = active_layers(&flat, tc(1.0));
    let mut wanted: Vec<(ClipId, f64)> = layers
        .iter()
        .flat_map(|l| l.clips.iter().map(move |c| (c, l.at, l.fps)))
        .filter(|(c, _, _)| matches!(c.kind, ClipKind::Media { .. }))
        .map(|(c, at, fps)| (c.id, klipp_compose::render::source_secs(c, at, fps)))
        .collect();
    wanted.sort_by(|a, b| a.1.total_cmp(&b.1));
    assert_eq!(wanted.len(), 2, "two instances, two media clips in play: {wanted:?}");
    assert_ne!(wanted[0].0, wanted[1].0, "under two ids");
    assert!((wanted[0].1 - 1.0).abs() < 1e-6 && (wanted[1].1 - 2.0).abs() < 1e-6, "each at its own source time: {wanted:?}");

    // And a source keyed by id is asked for both.
    let mut src = Flat::default();
    for (id, secs) in &wanted {
        let v = *secs as f32 / 4.0;
        src.0.insert(*id, Box::new(move || SourcePixels::Linear(Arc::new([v, v, v, 1.0].repeat((W * H) as usize)))));
    }
    for (i, img) in render(&flat, 1.0, &mut src).iter().enumerate() {
        assert_px(&format!("the top instance, a second further in, run {i}"), px(img, 16, 9), [0.25, 0.25, 0.25], 2e-3);
    }
}

/// A scene that contains itself renders nothing where the loop closes,
/// and the render returns.
#[test]
fn a_scene_that_contains_itself_renders_nothing_and_returns() {
    let mut a = Sequence::new_default();
    let mut b = Sequence::new_default();
    let mut at = Track::new("V1", TrackKind::Video);
    at.clips = vec![Clip::precomposition(b.id, tc(0.0), tc(4.0))];
    a.tracks = vec![at];
    let mut bt = Track::new("V1", TrackKind::Video);
    bt.clips = vec![Clip::precomposition(a.id, tc(0.0), tc(4.0))];
    b.tracks = vec![bt];
    let flat = klipp_timeline::flatten::flatten_sequence(&a, &[a.clone(), b.clone()]);
    for (i, img) in render(&flat, 1.0, &mut Flat::default()).iter().enumerate() {
        assert_px(&format!("nothing, run {i}"), px(img, 16, 9), [0.0, 0.0, 0.0], 1e-6);
    }
}

/// The viewer's fast path lays a title sprite over the presented bytes
/// in display space. Where the text is solid or absent that is the full
/// composite to the bit; only an anti-aliased edge may differ a little.

#[test]
fn a_title_laid_over_display_bytes_matches_the_full_composite() {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: 320, height: 180 };
    let mut text = Track::new("Text 1", TrackKind::Text);
    let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Over".into(), font_size: 200.0, ..Default::default() });
    let title = clip(ClipKind::Title(td), 0.0, 5.0);
    text.clips = vec![title.clone()];
    let mut video = Track::new("V1", TrackKind::Video);
    let m = media(0.0, 10.0);
    let mut src = Flat::default();
    src.0.insert(m.id, Box::new(|| SourcePixels::Linear(Arc::new([0.2, 0.1, 0.05, 1.0].repeat((320 * 180) as usize)))));
    video.clips = vec![m];
    seq.tracks = vec![text, video];
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    let full = klipp_effects::linear_f32_rgba_to_srgb_u8(&r.render(&seq, tc(1.0), 320, 180, &mut src, RenderOptions::default()));

    let mut video_only = seq.clone();
    video_only.tracks.remove(0);
    let mut fast = klipp_effects::linear_f32_rgba_to_srgb_u8(&r.render(&video_only, tc(1.0), 320, 180, &mut src, RenderOptions::default()));
    let sprite = r.render_title_layer_u8(&title, tc(1.0), 320, 180).expect("a title layer");
    klipp_compose::over_u8_straight(&mut fast, &sprite, 320);

    let (mut solid, mut clear, mut edge_worst) = (0usize, 0usize, 0i32);
    for ((f, s), a) in full.chunks_exact(4).zip(fast.chunks_exact(4)).zip(sprite.chunks_exact(4).map(|p| p[3])) {
        let d = (0..3).map(|i| (f[i] as i32 - s[i] as i32).abs()).max().unwrap();
        match a {
            255 => { solid += 1; assert!(d <= 1, "solid text pixel differs by {d}"); }
            0 => { clear += 1; assert!(d <= 1, "clear pixel differs by {d}"); }
            _ => edge_worst = edge_worst.max(d),
        }
    }
    assert!(solid > 200 && clear > 10_000, "solid {solid} clear {clear}");
    assert!(edge_worst <= 48, "edge pixels differ by up to {edge_worst}");
}

/// A clip placed smaller than the frame leaves a transparent margin, and
/// a shadow drawn into that margin is what the compositor lays over the
/// track below. This is the rule that a shadow runs *after* the
/// transform that made the margin: run before it, there is no margin to
/// draw in and the shadow is invisible.
#[test]
fn a_shadow_on_a_placed_layer_falls_on_the_track_below() {
    let mut top = media(0.0, 1.0);
    // Placed at two fifths of the frame by keyframes, as `compose`
    // writes it — so the transform is the one the renderer pushes.
    klipp_core::keyframe::upsert_keyframe(&mut top.transform_scale_keyframes, 0.0, 0.4);
    top.builtin_effects = vec![BuiltinEffect::DropShadow {
        offset_x: 0.0,
        offset_y: 0.06,
        blur: 0.05,
        color: [0, 0, 0],
        opacity: 1.0,
    }];
    let under = solid([255, 0, 0], 0.0, 1.0);
    let seq = sequence(vec![vec![top.clone()], vec![under]]);
    let mut src = Flat::default().linear(&top, [1.0, 1.0, 1.0]);

    for img in render(&seq, 0.5, &mut src) {
        let clear = px(&img, 1, 1);
        assert!(clear[0] > 0.9 && clear[1] < 0.05, "away from the layer it is the red below: {clear:?}");
        assert!(px(&img, 16, 9)[1] > 0.9, "the middle of the frame is the white layer");
        let shaded = px(&img, 16, 14);
        assert!(shaded[0] < clear[0] * 0.75, "the shadow darkens the background under the layer: {shaded:?} against {clear:?}");
    }
}

/// A camera over the sequence moves every layer at once, including one
/// that was never animated. Two clips sit side by side, placed by their
/// own keyframes; the camera looks left, and both move right together.
#[test]
fn a_camera_over_the_sequence_moves_every_layer_at_once() {
    let place = |c: &mut Clip, x: f32| {
        klipp_core::keyframe::upsert_keyframe(&mut c.transform_scale_keyframes, 0.0, 0.4);
        klipp_core::keyframe::upsert_keyframe(&mut c.transform_pos_x_keyframes, 0.0, x);
    };
    let (mut left, mut right) = (media(0.0, 1.0), media(0.0, 1.0));
    place(&mut left, -(W as f32) * 0.25);
    place(&mut right, (W as f32) * 0.25);

    let mut seq = sequence(vec![vec![left.clone()], vec![right.clone()]]);
    let mut src = Flat::default().linear(&left, [1.0, 0.0, 0.0]).linear(&right, [0.0, 1.0, 0.0]);

    // As composed, red is left of the middle and green right of it.
    for img in render(&seq, 0.5, &mut src) {
        assert!(px(&img, 8, 9)[0] > 0.5, "red sits left: {:?}", px(&img, 8, 9));
        assert!(px(&img, 24, 9)[1] > 0.5, "green sits right: {:?}", px(&img, 24, 9));
    }

    // The camera looks a quarter of the frame to the left, so the whole
    // scene slides a quarter of the frame right: red lands in the middle
    // and green leaves the frame, and neither clip was touched to do it.
    klipp_core::keyframe::upsert_keyframe(&mut seq.camera.x_keyframes, 0.0, 0.25);
    for img in render(&seq, 0.5, &mut src) {
        assert!(px(&img, 16, 9)[0] > 0.5, "red has moved to the middle: {:?}", px(&img, 16, 9));
        assert!(px(&img, 8, 9)[0] < 0.5, "and left where it was: {:?}", px(&img, 8, 9));
        assert!(px(&img, 24, 9)[1] < 0.5, "green has gone off the right: {:?}", px(&img, 24, 9));
    }
}

/// Depth reaches the render: under the same camera move, a layer on the
/// glass (depth 0) holds still while a layer on the composed plane moves,
/// and a layer three times as far moves a third as much.
#[test]
fn layers_at_different_depths_move_by_different_amounts_under_one_camera() {
    let place = |c: &mut Clip, x: f32, depth: f32| {
        klipp_core::keyframe::upsert_keyframe(&mut c.transform_scale_keyframes, 0.0, 0.25);
        klipp_core::keyframe::upsert_keyframe(&mut c.transform_pos_x_keyframes, 0.0, x);
        c.depth = depth;
    };
    let (mut plane, mut glass, mut far) = (media(0.0, 1.0), media(0.0, 1.0), media(0.0, 1.0));
    place(&mut plane, -8.0, 1.0);
    place(&mut glass, 8.0, 0.0);
    place(&mut far, 0.0, 3.0);
    // Below the other two, so neither covers it.
    klipp_core::keyframe::upsert_keyframe(&mut far.transform_pos_y_keyframes, 0.0, 5.0);
    let mut seq = sequence(vec![vec![plane.clone()], vec![glass.clone()], vec![far.clone()]]);
    // Looking a quarter of the frame left: the plane moves 8 px right.
    klipp_core::keyframe::upsert_keyframe(&mut seq.camera.x_keyframes, 0.0, 0.25);
    let mut src = Flat::default()
        .linear(&plane, [1.0, 0.0, 0.0])
        .linear(&glass, [0.0, 1.0, 0.0])
        .linear(&far, [0.0, 0.0, 1.0]);
    let centre = |img: &[f32], ch: usize| {
        let row = if ch == 2 { 14 } else { 9 };
        let xs: Vec<u32> = (0..W).filter(|&x| px(img, x, row)[ch] > 0.5).collect();
        xs.iter().sum::<u32>() as f32 / xs.len().max(1) as f32
    };
    for img in render(&seq, 0.5, &mut src) {
        let (red, green, blue) = (centre(&img, 0), centre(&img, 1), centre(&img, 2));
        assert!((red - 15.5).abs() < 1.0, "the plane moved to the middle: {red}");
        assert!((green - 23.5).abs() < 1.0, "the glass held still: {green}");
        // The far layer sat in the middle; it moves a third of 8 px.
        assert!(blue > 15.5 && blue < 19.0, "the far layer moved about 2.7 px: {blue}");
    }
}

/// A flat white frame at any size asked for.
struct White;
impl FrameSource for White {
    fn frame(&mut self, _clip: &Clip, _secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new([1.0, 1.0, 1.0, 1.0].repeat((width * height) as usize))), width, height, pts: None })
    }
}

/// A card tilted about its vertical axis is drawn in perspective: the
/// edge swung away is shorter than the edge swung near. Positive tilt
/// swings the right edge away. Tilted about its horizontal axis, the top
/// leans away and is narrower than the bottom.
#[test]
fn a_tilted_card_is_drawn_in_perspective_the_way_its_tilt_says() {
    let (w, h) = (128u32, 72u32);
    let render_card = |tx: f32, ty: f32| {
        let mut card = media(0.0, 1.0);
        klipp_core::keyframe::upsert_keyframe(&mut card.transform_scale_keyframes, 0.0, 0.5);
        klipp_core::keyframe::upsert_keyframe(&mut card.transform_tilt_x_keyframes, 0.0, tx);
        klipp_core::keyframe::upsert_keyframe(&mut card.transform_tilt_y_keyframes, 0.0, ty);
        let seq = sequence(vec![vec![card]]);
        [false, true].map(|gpu| {
            let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            r.set_gpu_enabled(gpu);
            r.render(&seq, tc(0.5), w, h, &mut White, RenderOptions::default())
        })
    };
    let lit = |img: &[f32], x: u32, y: u32| img[((y * w + x) * 4) as usize] > 0.5;
    let column = |img: &[f32], x: u32| (0..h).filter(|&y| lit(img, x, y)).count();
    let row = |img: &[f32], y: u32| (0..w).filter(|&x| lit(img, x, y)).count();
    let lit_cols = |img: &[f32]| (0..w).filter(|&x| column(img, x) > 0).collect::<Vec<_>>();
    let lit_rows = |img: &[f32]| (0..h).filter(|&y| row(img, y) > 0).collect::<Vec<_>>();

    for img in render_card(0.0, 40.0) {
        let cols = lit_cols(&img);
        let (l, r) = (cols[1], cols[cols.len() - 2]);
        eprintln!("tilt_y 40: left edge {} px tall, right edge {} px tall, {} wide", column(&img, l), column(&img, r), cols.len());
        assert!(column(&img, r) < column(&img, l), "the right edge swung away is shorter");
        // Flat, the card is 64 x 36. The picture is tilted at full size,
        // seen through a pinhole a frame's width away, then scaled: turned
        // 40 degrees it foreshortens to about 55 wide, its near edge 53
        // tall and its far one 27. Rotating the screen point forward, as
        // before, drew it 94 wide.
        assert!((cols.len() as i32 - 55).abs() <= 3, "narrower than flat, not wider: {}", cols.len());
        assert!((column(&img, l) as i32 - 53).abs() <= 3 && (column(&img, r) as i32 - 27).abs() <= 3);
    }
    // A tilt set still, on the clip's own Transform with no perspective
    // given, is drawn in perspective too, not squashed flat.
    let mut card = media(0.0, 1.0);
    card.builtin_effects = vec![BuiltinEffect::Transform {
        position_x: 0.0, position_y: 0.0, scale_x: 0.5, scale_y: 0.5, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5,
        rotation_x: 0.0, rotation_y: 40.0, perspective: 0.0, focal_ref: None,
    }];
    let seq = sequence(vec![vec![card]]);
    for gpu in [false, true] {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(gpu);
        let img = r.render(&seq, tc(0.5), w, h, &mut White, RenderOptions::default());
        let cols = lit_cols(&img);
        let (l, r) = (cols[1], cols[cols.len() - 2]);
        assert!(column(&img, r) + 10 < column(&img, l), "a still tilt leans in perspective: {} vs {}", column(&img, l), column(&img, r));
    }

    for img in render_card(40.0, 0.0) {
        let rows = lit_rows(&img);
        let (t, b) = (rows[1], rows[rows.len() - 2]);
        eprintln!("tilt_x 40: top {} px wide, bottom {} px wide, {} tall", row(&img, t), row(&img, b), rows.len());
        assert!(row(&img, t) < row(&img, b), "the top leaning away is narrower");
        assert!(rows.len() < 36, "leaning back, it is shorter than flat: {}", rows.len());
    }
}

/// A clip carried by a link smears like one that moves itself.
///
/// Issue #195: a child following an animated parent had no keyframes of
/// its own, so nothing asked what carried it and it never reached the
/// blur path; and even reaching it, the shutter had no transform to
/// sample, because the child's position at an instant is whatever its
/// target was at that instant. A card-and-label move came out with the
/// card smeared and the label sharp.
///
/// The comparison the issue proposes: the same motion, once followed
/// and once written on the clip, must smear the same way.
#[test]
fn a_linked_clip_is_smeared_by_what_carries_it() {
    let (w, h) = (128u32, 72u32);
    // The mover is a speck at the centre of the frame — it cannot be
    // moved out of the way, because the card inherits its position and
    // would go with it — so the row measured is one the card covers and
    // the speck does not.
    let shot = |linked: bool| {
        let mut parent = media(0.0, 1.0);
        klipp_core::keyframe::upsert_keyframe(&mut parent.transform_scale_keyframes, 0.0, 0.01);
        klipp_core::keyframe::upsert_keyframe(&mut parent.transform_pos_x_keyframes, 0.0, -48.0);
        klipp_core::keyframe::upsert_keyframe(&mut parent.transform_pos_x_keyframes, 1.0, 48.0);

        let mut card = media(0.0, 1.0);
        klipp_core::keyframe::upsert_keyframe(&mut card.transform_scale_keyframes, 0.0, 0.25);
        if linked {
            card.transform_link = Some(klipp_timeline::TransformLink {
                target: klipp_timeline::TransformLinkTarget::Clip(parent.id),
                mode: klipp_timeline::LinkMode::default(),
                inherit_scale: false,
                inherit_position: true,
                inherit_rotation: false,
                inherit_anchor: false,
            });
        } else {
            // The same path, written on the clip itself.
            klipp_core::keyframe::upsert_keyframe(&mut card.transform_pos_x_keyframes, 0.0, -48.0);
            klipp_core::keyframe::upsert_keyframe(&mut card.transform_pos_x_keyframes, 1.0, 48.0);
        }
        let mut seq = sequence(vec![vec![card], vec![parent]]);
        seq.camera.shutter_deg = 360.0;
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(false);
        r.render(&seq, tc(0.5), w, h, &mut White, RenderOptions::default())
    };
    let soft = |img: &[f32]| (0..w).filter(|&x| (0.05..0.95).contains(&img[((28 * w + x) * 4) as usize])).count();
    let (followed, written) = (shot(true), shot(false));
    assert!(
        soft(&followed) >= 6,
        "a card carried by an animated parent should smear, got {} soft pixels",
        soft(&followed),
    );
    assert!(
        (soft(&followed) as i64 - soft(&written) as i64).abs() <= 2,
        "followed and written motion should smear alike: {} against {}",
        soft(&followed),
        soft(&written),
    );
}

/// With the shutter open, a card moving fast is smeared along its path:
/// its leading and trailing edges fade over the distance it covers while
/// the shutter is open, where without it they are a pixel sharp. A card
/// that does not move stays sharp either way.
#[test]
fn a_moving_card_is_smeared_along_its_path_while_the_shutter_is_open() {
    let (w, h) = (128u32, 72u32);
    let shot = |shutter: f32, moving: bool| {
        let mut card = media(0.0, 1.0);
        klipp_core::keyframe::upsert_keyframe(&mut card.transform_scale_keyframes, 0.0, 0.25);
        klipp_core::keyframe::upsert_keyframe(&mut card.transform_pos_x_keyframes, 0.0, -48.0);
        klipp_core::keyframe::upsert_keyframe(&mut card.transform_pos_x_keyframes, 1.0, if moving { 48.0 } else { -48.0 });
        let mut seq = sequence(vec![vec![card]]);
        seq.camera.shutter_deg = shutter;
        [false, true].map(|gpu| {
            let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            r.set_gpu_enabled(gpu);
            r.render(&seq, tc(0.5), w, h, &mut White, RenderOptions::default())
        })
    };
    // Pixels on the middle row that are neither background nor card.
    let soft = |img: &[f32]| (0..w).filter(|&x| (0.05..0.95).contains(&img[((36 * w + x) * 4) as usize])).count();
    // 96 px a second at 24 fps is 4 px a frame; a 360 degree shutter
    // smears each edge across about that.
    for (sharp, blurred) in shot(0.0, true).iter().zip(shot(360.0, true).iter()) {
        assert!(soft(sharp) <= 2, "no shutter, sharp edges: {}", soft(sharp));
        assert!(soft(blurred) >= 6, "shutter open, both edges smeared: {}", soft(blurred));
    }
    for still in shot(360.0, false) {
        assert!(soft(&still) <= 2, "a card that does not move stays sharp: {}", soft(&still));
    }

    // A card that holds still but is carried by the camera is smeared too.
    let mut card = media(0.0, 1.0);
    klipp_core::keyframe::upsert_keyframe(&mut card.transform_scale_keyframes, 0.0, 0.25);
    let mut seq = sequence(vec![vec![card]]);
    seq.camera.shutter_deg = 360.0;
    klipp_core::keyframe::upsert_keyframe(&mut seq.camera.x_keyframes, 0.0, 0.1);
    klipp_core::keyframe::upsert_keyframe(&mut seq.camera.x_keyframes, 1.0, 0.9);
    for gpu in [false, true] {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(gpu);
        let img = r.render(&seq, tc(0.5), w, h, &mut White, RenderOptions::default());
        assert!(soft(&img) >= 6, "carried by the camera, smeared: {}", soft(&img));
    }
}
