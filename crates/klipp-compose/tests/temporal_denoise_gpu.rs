//! Temporal noise reduction on the GPU paths (#996). It runs on the
//! source at the head of the CPU render, and `clip_effects` lifts it out
//! of the effect chain, so a GPU path that took the clip saw no effect
//! at all and rendered it noisy: the viewer's GPU stack and the export's
//! single-clip path both did. They hand such a clip to the CPU now, and
//! these hold them to the CPU's picture.

use std::sync::Arc;

use klipp_compose::{FrameSource, LinearFrameProviderFn, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, Timecode, WorkingColorspace};
use klipp_effects::BuiltinEffect;
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams, NodePos};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

/// A multiple of 8 wide and even high, as the GPU export path needs.
const W: u32 = 32;
const H: u32 = 16;
const FPS: f64 = 24.0;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

/// A grey with grain that changes every frame: what averaging a frame
/// with its neighbours visibly cleans.
fn grainy(source_secs: f64) -> Vec<f32> {
    let n = (source_secs * FPS).round() as i64;
    (0..W * H)
        .flat_map(|i| {
            let h = (i as i64 * 7919 + n * 104_729).rem_euclid(1000) as f32 / 1000.0;
            let v = 0.2 + (h - 0.5) * 0.08;
            [v, v, v, 1.0]
        })
        .collect()
}

struct Grain;

impl FrameSource for Grain {
    fn frame(&mut self, _clip: &Clip, source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(grainy(source_secs))), width, height, pts: None })
    }

    fn temporal_frames_linear(&mut self, clip: &Clip, _working: WorkingColorspace) -> Option<LinearFrameProviderFn> {
        let source_in = clip.source_in.to_secs_f64();
        Some(Box::new(move |clip_secs| Some((grainy(source_in + clip_secs), W, H))))
    }
}

/// A media clip, graded a little so the export's single-clip path runs
/// its grade rather than passing the frame through.
fn clip(denoise: bool) -> Clip {
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
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
        node(NodeKind::Exposure, NodeParams { exposure: Some(0.25), ..Default::default() }),
        node(NodeKind::Output, NodeParams::default()),
    ];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
    c.node_graph = NodeGraph {
        output_node_id: nodes[2].id,
        nodes,
        edges,
        node_positions,
        working_space_override: None,
        frames: vec![],
        sticky_notes: vec![],
    };
    if denoise {
        c.builtin_effects = vec![BuiltinEffect::TemporalDenoise { strength: 1.0, threshold: 0.2 }];
    }
    c
}

fn sequence(c: Clip) -> Sequence {
    let mut seq = Sequence::new_default();
    let mut t = Track::new("V1", TrackKind::Video);
    t.clips = vec![c];
    seq.tracks = vec![t];
    seq
}

fn render(seq: &Sequence, gpu: bool) -> Vec<f32> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.render(seq, tc(0.5), W, H, &mut Grain, RenderOptions::default())
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

/// The fixture is worth something only if denoising changes it well past
/// the tolerance the paths are held to.
#[test]
fn the_cpu_render_denoises_the_grain() {
    let noisy = render(&sequence(clip(false)), false);
    let clean = render(&sequence(clip(true)), false);
    assert!(worst(&noisy, &clean) > 0.01, "denoising changed nothing ({})", worst(&noisy, &clean));
}

/// The viewer's GPU stack shows the CPU's picture, denoised: it hands
/// the clip to the CPU, or matches it within a display level.
#[test]
fn the_gpu_stack_shows_the_denoised_picture() {
    let seq = sequence(clip(true));
    let display = |linear: &[f32]| {
        klipp_color::display::display_transform_ws_to(
            linear,
            klipp_color::display::ToneMapOp::None,
            WorkingColorspace::Rec709Linear,
            klipp_core::SdrTransfer::Srgb,
        )
    };
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(0.5), W, H, &mut Grain, RenderOptions::default()) else {
        return;
    };
    let cpu = display(&render(&seq, false));
    let off = cpu.iter().zip(&gpu).map(|(c, g)| (*c as i32 - *g as i32).abs()).max().unwrap_or(0);
    assert!(off <= 1, "the GPU stack differs from the denoised render by {off} levels");
}

/// The export's GPU path declines the clip, so the export renders it on
/// the CPU, denoised; were it to take it, its luma would have to be the
/// CPU picture's.
#[test]
fn the_export_gpu_path_does_not_skip_the_denoise() {
    let seq = sequence(clip(true));
    let target = klipp_effects::gpu::Yuv420Target {
        tone_map_op: 0,
        odt_tag: 0,
        scene: None,
        gamut_op: 0,
        hdr_op: 0,
        peak_nits: 0.0,
        luma_coefs: [0.2126, 0.7152, 0.0722],
        semi_planar: false,
        bits: 8,
        sample_shift: 0,
    };
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    let Some((planes, _)) = r.render_yuv420_on_gpu(&seq, tc(0.5), W, H, &mut Grain, RenderOptions::default(), target) else {
        return;
    };
    let display = klipp_color::display::ExportDisplay::new(
        klipp_core::ToneMapOp::None,
        WorkingColorspace::Rec709Linear,
        klipp_core::OdtChoice::MatchViewer,
    );
    let want: Vec<u8> = render(&seq, false)
        .chunks_exact(4)
        .map(|p| {
            let e = display.encode([p[0], p[1], p[2]]);
            let y = 0.2126 * e[0] + 0.7152 * e[1] + 0.0722 * e[2];
            (16.0 + y * 219.0 + 0.5).clamp(0.0, 255.0) as u8
        })
        .collect();
    let off = planes[..want.len()].iter().zip(&want).map(|(g, w)| (*g as i32 - *w as i32).abs()).max().unwrap_or(0);
    assert!(off <= 1, "the GPU export's luma differs from the denoised render by {off} levels");
}
