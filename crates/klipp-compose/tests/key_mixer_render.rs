//! A Key Mixer (#898) keying a Corrector renders on the timeline as its
//! mix says: two windows apart, one at the left and one at the right,
//! mixed at gain 1 each grade both; the right one's gain at 0 leaves it
//! ungraded; outside both is ungraded throughout. CPU paths only and GPU
//! (the viewer's and the export's renderer) alike.

use std::collections::HashMap;
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ClipId, Mask, MaskShape, MaskStack, RationalTime, Timecode};
use klipp_nodes::{Edge, KeyMixerInput, KeyMixerParams, NodeGraph, NodeKind};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

const W: u32 = 40;
const H: u32 = 20;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

#[derive(Default)]
struct Grey(HashMap<ClipId, f32>);

impl FrameSource for Grey {
    fn frame(&mut self, clip: &Clip, _source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let v = *self.0.get(&clip.id)?;
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new([v, v, v, 1.0].repeat((width * height) as usize))), width, height, pts: None })
    }
}

fn node(kind: NodeKind, params: klipp_nodes::NodeParams) -> klipp_nodes::Node {
    klipp_nodes::Node { id: klipp_nodes::NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() }
}

/// A clip whose Corrector lifts a stop, keyed by a Key Mixer of a left
/// and a right window at `gains`; ungraded with `None`.
fn clip(gains: Option<[f32; 2]>) -> Clip {
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(4.0)) };
    let mut g = NodeGraph::default();
    let corrector = g.ensure_corrector();
    if let Some(gains) = gains {
        g.nodes.iter_mut().find(|n| n.id == corrector).unwrap().params.exposure = Some(1.0);
        let source = g.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id;
        let inputs = gains.iter().map(|&gain| KeyMixerInput { gain, invert: false }).collect();
        let mixer = node(NodeKind::KeyMixer, klipp_nodes::NodeParams { key_mixer: Some(KeyMixerParams { inputs, ..Default::default() }), ..Default::default() });
        let km = mixer.id;
        for (port, x) in [0.2f32, 0.8].into_iter().enumerate() {
            let circle = MaskStack { masks: vec![Mask { shape: MaskShape::Circle { center_x: x, center_y: 0.5, radius: 0.15 }, feather: 0.0, ..Mask::default() }] };
            let w = node(NodeKind::Mask, klipp_nodes::NodeParams { masks: Some(circle), ..Default::default() });
            g.edges.push(Edge { from_node: source, from_port: 0, to_node: w.id, to_port: 0 });
            g.edges.push(Edge { from_node: w.id, from_port: 0, to_node: km, to_port: port as u8 });
            g.nodes.push(w);
        }
        g.nodes.push(mixer);
        g.edges.push(Edge { from_node: km, from_port: 0, to_node: corrector, to_port: 1 });
    }
    c.set_grade(g);
    c
}

/// The left window's centre, the right one's and the middle (in neither),
/// CPU paths only and GPU.
fn render(clip: &Clip) -> [[f32; 3]; 2] {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V1", TrackKind::Video);
    track.clips = vec![clip.clone()];
    seq.tracks = vec![track];
    let mut src = Grey::default();
    src.0.insert(clip.id, 0.18);
    [false, true].map(|gpu| {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(gpu);
        let img = r.render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default());
        [W * 2 / 10, W * 8 / 10, W / 2].map(|x| img[(((H / 2) * W + x) * 4) as usize])
    })
}

#[test]
fn a_key_mixer_keys_a_correction_as_its_mix_says() {
    let [plain, _] = render(&clip(None));
    let [both_cpu, both_gpu] = render(&clip(Some([1.0, 1.0])));
    let [left_cpu, left_gpu] = render(&clip(Some([1.0, 0.0])));
    let lifted = both_cpu[0];
    assert!(lifted > plain[0] * 1.5, "the left window grades: {lifted} over {}", plain[0]);
    let close = |a: f32, b: f32| (a - b).abs() < 2e-3;
    assert!(close(both_cpu[1], lifted) && close(both_cpu[2], plain[2]), "both mixed in: the right graded, the middle not: {both_cpu:?}");
    assert!(close(left_cpu[0], lifted) && close(left_cpu[1], plain[1]) && close(left_cpu[2], plain[2]), "the right at gain 0 is left out: {left_cpu:?}");
    for (cpu, gpu) in [(both_cpu, both_gpu), (left_cpu, left_gpu)] {
        assert!(cpu.iter().zip(&gpu).all(|(a, b)| close(*a, *b)), "CPU {cpu:?} and GPU {gpu:?} agree");
    }
}

/// A horizontal ramp, so a key and a matte read from the picture vary.
struct Ramp(ClipId);

impl FrameSource for Ramp {
    fn frame(&mut self, clip: &Clip, _source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        (clip.id == self.0).then(|| SourceFrame {
            pixels: SourcePixels::Linear(Arc::new((0..width * height).flat_map(|i| {
                let v = 0.05 + 0.9 * (i % width) as f32 / width as f32;
                [v, v * 0.8, v * 0.6, 1.0]
            }).collect())),
            width,
            height,
            pts: None,
        })
    }
}

/// The whole frame alike on the CPU route and on the GPU stack, for a Key
/// Mixer whose inputs are a feathered window, a luma key and the picture
/// itself wired straight in (read as its red, as a matte is). A run
/// seeded on the GPU, where the picture has no CPU pixels, is held by
/// klipp-nodes `key_mixer::a_gpu_seeded_source_into_a_key_mixer_is_never_taken_for_everything`.
#[test]
fn a_key_mixer_draws_the_same_frame_on_both_routes() {
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(4.0)) };
    let mut g = NodeGraph::default();
    let corrector = g.ensure_corrector();
    g.nodes.iter_mut().find(|n| n.id == corrector).unwrap().params.exposure = Some(1.0);
    let source = g.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id;
    let inputs = vec![KeyMixerInput { gain: 0.6, invert: false }, KeyMixerInput { gain: 0.5, invert: true }, KeyMixerInput { gain: -0.4, invert: false }];
    let mixer = node(NodeKind::KeyMixer, klipp_nodes::NodeParams { key_mixer: Some(KeyMixerParams { inputs, output_gain: 1.2, output_offset: 0.05 }), ..Default::default() });
    let km = mixer.id;
    let window = node(NodeKind::Mask, klipp_nodes::NodeParams {
        masks: Some(MaskStack { masks: vec![Mask { shape: MaskShape::Circle { center_x: 0.4, center_y: 0.5, radius: 0.3 }, feather: 0.25, ..Mask::default() }] }),
        ..Default::default()
    });
    let key = node(NodeKind::LumaKey, klipp_nodes::NodeParams {
        luma_key: Some(klipp_core::color_grade::LumaKeySettings { enabled: true, low: 0.3, high: 0.7, soft: 0.2, ..Default::default() }),
        ..Default::default()
    });
    for (port, from) in [(0u8, window.id), (1, key.id), (2, source)] {
        if from != source {
            g.edges.push(Edge { from_node: source, from_port: 0, to_node: from, to_port: 0 });
        }
        g.edges.push(Edge { from_node: from, from_port: 0, to_node: km, to_port: port });
    }
    g.nodes.extend([window, key, mixer]);
    g.edges.push(Edge { from_node: km, from_port: 0, to_node: corrector, to_port: 1 });
    c.set_grade(g);
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V1", TrackKind::Video);
    track.clips = vec![c.clone()];
    seq.tracks = vec![track];
    let mut src = Ramp(c.id);
    let display = |linear: &[f32]| klipp_color::display::display_transform_ws_to(linear, klipp_color::display::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb);
    let worst = |a: &[u8], b: &[u8]| a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0);
    let mut cpu_only = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    cpu_only.set_gpu_enabled(false);
    let cpu = display(&cpu_only.render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()));
    // The mix keys part of the frame: graded somewhere, not everywhere.
    let mut plain = seq.clone();
    plain.tracks[0].clips[0].edit_grade(|g| g.edges.retain(|e| e.to_node != corrector || e.to_port != 1));
    let lifted = display(&cpu_only.render(&plain, tc(1.0), W, H, &mut src, RenderOptions::default()));
    let mut ungraded = plain.clone();
    ungraded.tracks[0].clips[0].edit_grade(|g| g.nodes.iter_mut().filter(|n| n.id == corrector).for_each(|n| n.params.exposure = None));
    let flat = display(&cpu_only.render(&ungraded, tc(1.0), W, H, &mut src, RenderOptions::default()));
    assert!(worst(&cpu, &lifted) > 8 && worst(&cpu, &flat) > 8, "the key grades part of the frame");
    let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    gpu.set_gpu_enabled(true);
    let timeline = display(&gpu.render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()));
    assert!(worst(&cpu, &timeline) <= 1, "the GPU-enabled render: {} levels from the CPU", worst(&cpu, &timeline));
    match gpu.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) {
        Some((stack, _)) => assert!(worst(&cpu, &stack) <= 2, "the stack: {} levels from the CPU", worst(&cpu, &stack)),
        None => {
            assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but the stack declined: {:?}", gpu.last_decline());
            eprintln!("UNVERIFIED: the stack declined ({:?})", gpu.last_decline());
        }
    }
}
