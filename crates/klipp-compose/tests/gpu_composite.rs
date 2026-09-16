//! The GPU stack's frame is the CPU renderer's frame: the same layers,
//! blends and opacities, encoded to display bytes, within a level.

use std::collections::HashMap;
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ClipId, RationalTime, Timecode};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 320;
const H: u32 = 180;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn clip(kind: ClipKind, from: f64, to: f64) -> Clip {
    Clip { kind, ..Clip::gap(tc(from), tc(to)) }
}

#[derive(Default)]
struct Flat(HashMap<ClipId, Arc<Vec<f32>>>);

impl FrameSource for Flat {
    fn frame(&mut self, clip: &Clip, _source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        self.0.get(&clip.id).map(|p| SourceFrame { pixels: SourcePixels::Linear(Arc::clone(p)), width, height, pts: None })
    }
}

fn picture(seed: f32) -> Arc<Vec<f32>> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let fx = x as f32 / (W - 1) as f32;
            let fy = y as f32 / (H - 1) as f32;
            v.extend_from_slice(&[(fx + seed).fract() * 0.8, fy * 0.6, ((fx + fy) * 0.5 + seed).fract() * 0.7, 1.0]);
        }
    }
    Arc::new(v)
}

fn display(linear: &[f32]) -> Vec<u8> {
    klipp_color::display::display_transform_ws_to(
        linear,
        klipp_color::display::ToneMapOp::None,
        klipp_core::WorkingColorspace::Rec709Linear,
        klipp_core::SdrTransfer::Srgb,
    )
}

fn worst(a: &[u8], b: &[u8]) -> i32 {
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0)
}

/// Two video layers with a blend mode and opacity, a title, a generator.
fn stack() -> (Sequence, Flat) {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut text = Track::new("Text", TrackKind::Text);
    let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Stack".into(), font_size: 160.0, ..Default::default() });
    text.clips = vec![clip(ClipKind::Title(td), 0.0, 5.0)];
    let mut gen = Track::new("Gen", TrackKind::Video);
    let mut g = clip(ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color: [255, 80, 20, 255] }), 0.0, 5.0);
    g.blend_mode = klipp_core::BlendMode::Multiply;
    g.builtin_effects = vec![klipp_effects::BuiltinEffect::Opacity { value: 0.4 }];
    gen.clips = vec![g];
    let mut top = Track::new("V2", TrackKind::Video);
    let mut t = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 10.0);
    t.blend_mode = klipp_core::BlendMode::Screen;
    t.builtin_effects = vec![klipp_effects::BuiltinEffect::Opacity { value: 0.5 }];
    let mut base = Track::new("V1", TrackKind::Video);
    let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 10.0);
    let mut src = Flat::default();
    src.0.insert(t.id, picture(0.3));
    src.0.insert(b.id, picture(0.0));
    top.clips = vec![t];
    base.clips = vec![b];
    seq.tracks = vec![text, gen, top, base];
    (seq, src)
}

#[test]
fn the_gpu_stack_is_the_renderers_frame() {
    let (seq, mut src) = stack();
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) else {
        eprintln!("skipping: the GPU stack declined (no usable GPU?)");
        return;
    };
    let cpu = display(&r.render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()));
    let d = worst(&cpu, &gpu);
    assert!(d <= 1, "GPU stack differs from the CPU renderer by {d} levels");
}

#[test]
fn the_gpu_stack_declines_what_it_cannot_do_yet() {
    let (seq, mut src) = stack();
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let hdr = RenderOptions { hdr_peak_nits: Some(1000.0), ..Default::default() };
    assert!(r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, hdr).is_none());
}

fn renderer() -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    r
}

/// Both paths on `seq` at 1 s, as display bytes; `None` when the GPU
/// stack declined.
fn both(seq: &Sequence, src: &mut Flat) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut r = renderer();
    let (gpu, _) = r.render_display_u8_on_gpu(seq, tc(1.0), W, H, src, RenderOptions::default())?;
    let cpu = display(&r.render(seq, tc(1.0), W, H, src, RenderOptions::default()));
    Some((cpu, gpu))
}

fn assert_match(name: &str, seq: &Sequence, src: &mut Flat) {
    let Some((cpu, gpu)) = both(seq, src) else {
        eprintln!("skipping {name}: the GPU stack declined (no usable GPU?)");
        return;
    };
    let d = worst(&cpu, &gpu);
    assert!(d <= 1, "{name}: GPU stack differs from the CPU renderer by {d} levels");
}

#[test]
fn a_luma_matte_with_feather_and_choke_matches() {
    let (mut seq, mut src) = stack();
    let source = seq.tracks[3].clips[0].id;
    seq.tracks[2].clips[0].matte_source = Some(klipp_timeline::MatteBinding {
        source_clip_id: source,
        channel: klipp_nodes::params::MatteChannel::Luminance,
        invert: true,
        feather_px: 3.0,
        choke_px: 2.0,
    });
    seq.tracks[3].clips[0].consumed_as_matte = true;
    assert_match("luma matte", &seq, &mut src);
}

#[test]
fn a_stencil_masks_the_layers_below_it() {
    let (mut seq, mut src) = stack();
    // The generator becomes a stencil over the two video layers.
    seq.tracks[1].clips[0].matte_stencil = Some(klipp_timeline::StencilSettings {
        channel: klipp_nodes::params::MatteChannel::Alpha,
        invert: false,
        feather_px: 0.0,
        choke_px: 0.0,
    });
    assert_match("stencil", &seq, &mut src);
}

#[test]
fn a_transition_between_two_clips_matches() {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut v = Track::new("V1", TrackKind::Video);
    let a = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 2.0);
    let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 2.0, 4.0);
    let mut src = Flat::default();
    src.0.insert(a.id, picture(0.0));
    src.0.insert(b.id, picture(0.5));
    v.transitions.push(klipp_timeline::Transition {
        id: klipp_core::TransitionId::new(),
        clip_a_id: a.id,
        clip_b_id: b.id,
        kind: klipp_core::TransitionKind::CrossDissolve,
        duration: tc(2.0),
        alignment: Default::default(),
        user_params: Default::default(),
    });
    v.clips = vec![a, b];
    seq.tracks = vec![v];
    assert_match("cross dissolve", &seq, &mut src);
}

#[test]
fn an_adjustment_layer_with_a_grade_and_an_effect_matches() {
    let (mut seq, mut src) = stack();
    let mut adj = Track::new("Adj", TrackKind::Video);
    let mut a = clip(ClipKind::AdjustmentLayer, 0.0, 5.0);
    a.node_graph = {
        use klipp_nodes::{graph::{Edge, Node, NodeGraph, NodeId, NodePos}, kinds::NodeKind, params::NodeParams};
        let node = |kind: NodeKind, params: NodeParams| Node {
            id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![],
        };
        let nodes = vec![
            node(NodeKind::Source, NodeParams::default()),
            node(NodeKind::Exposure, NodeParams { exposure: Some(-0.5), ..Default::default() }),
            node(NodeKind::Output, NodeParams::default()),
        ];
        let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
        let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
        NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
    };
    a.builtin_effects = vec![klipp_effects::BuiltinEffect::GaussianBlur { radius: 2.0 }, klipp_effects::BuiltinEffect::Opacity { value: 0.7 }];
    adj.clips = vec![a];
    seq.tracks.insert(1, adj);
    assert_match("adjustment layer", &seq, &mut src);
}

#[test]
fn a_title_with_an_effect_and_a_faded_layer_match() {
    let (mut seq, mut src) = stack();
    seq.tracks[0].clips[0].builtin_effects = vec![klipp_effects::BuiltinEffect::GaussianBlur { radius: 1.5 }];
    seq.tracks[2].opacity = 0.6;
    assert_match("title effect", &seq, &mut src);
}

/// The timeline's LUT over the stack: applied on the accumulator before
/// the premultiply, where `render_stack` applies it.
#[test]
fn a_global_lut_over_the_stack_matches() {
    let (seq, mut src) = stack();
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
    let lut = klipp_color::lut::LutData {
        size,
        data,
        input_space: klipp_core::WorkingColorspace::Rec709Linear,
        output_space: klipp_core::WorkingColorspace::Rec709Linear,
    };
    let mut r = renderer();
    r.set_global_lut(Some(klipp_compose::GlobalLut { lut: lut.clone(), opacity: 0.9 }));
    let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) else {
        eprintln!("skipping: the GPU stack declined (no usable GPU?)");
        return;
    };
    let cpu = display(&r.render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()));
    let d = worst(&cpu, &gpu);
    assert!(d <= 1, "LUT over the stack: GPU differs from the CPU renderer by {d} levels");
    let mut plain = renderer();
    let (no_lut, _) = plain.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()).unwrap();
    assert!(worst(&no_lut, &gpu) > 20, "the LUT is not reaching the stack's frame");
}
