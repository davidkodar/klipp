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
    let mut p = NodeParams::default();
    p.exposure = Some(stops);
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

/// Temperature and tint are a white balance: they scale the channels, so a
/// grey four times brighter is balanced exactly as the dim one, black stays
/// black, and it is the same on the CPU and the GPU, in the Corrector and
/// the Color Balance node. They used to add a fixed amount, which tinted
/// blacks and did next to nothing to scene-linear highlights.
#[test]
fn temperature_and_tint_balance_every_exposure_alike() {
    for kind in [NodeKind::Corrector, NodeKind::ColorBalance] {
        let mut p = NodeParams::default();
        p.temperature = Some(40.0);
        p.tint = Some(-15.0);
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
    let mut p = NodeParams::default();
    p.exposure = Some(0.0);
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
