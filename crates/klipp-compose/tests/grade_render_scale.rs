//! A grade looks the same at any render size (#1012). Five grade
//! settings are measured in pixels: a qualifier's shrink/grow and blur,
//! the denoise radius, the grain's size and the clarity blur. They are
//! authored at the sequence's size, and a half-size render (reduced
//! playback quality) used to apply them in its own pixels, so each
//! covered twice as much of the picture as in the export.
//!
//! Each case renders a clip at the sequence's size and at half of it.
//! The full render shrunk 2x is what the half render should look like.
//! The half render of the old behaviour is the same grade on a sequence
//! that is itself half the size (settings in its own pixels): the
//! scaled grade has to be clearly closer to the shrunk full render than
//! that, and close in its own right.

use klipp_timeline::GradeAccess;
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{FilmGrainSettings, QualifierSettings, RationalTime, Resolution};
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 192;
const H: u32 = 108;

/// Hue across, brightness down, with hard-edged blocks so a blur, a
/// shrink or a clarity pass has edges to act on. Drawn in the frame's
/// own coordinates, so the half-size frame is the full one shrunk.
struct Chart;

impl FrameSource for Chart {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (u, t) = (x as f32 / w as f32, y as f32 / h as f32);
                let block = if ((u * 8.0) as u32 + (t * 4.0) as u32).is_multiple_of(2) { 1.0 } else { 0.55 };
                let hue = u * 6.0;
                let f = |n: f32| {
                    let k = (n + hue) % 6.0;
                    1.0 - (k.min(4.0 - k).clamp(0.0, 1.0)) * 0.7
                };
                let level = (0.15 + 0.7 * (1.0 - t)) * block;
                v.extend_from_slice(&[f(5.0) * level, f(3.0) * level, f(1.0) * level, 1.0]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn graph(kind: NodeKind, params: NodeParams) -> NodeGraph {
    let node = |kind, params| Node {
        id: NodeId::new_v4(),
        kind,
        label: String::new(),
        enabled: true,
        params,
        keyframes: Default::default(),
        output_link: None,
        input_links: vec![],
    };
    let nodes = vec![node(NodeKind::Source, NodeParams::default()), node(kind, params), node(NodeKind::Output, NodeParams::default())];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    NodeGraph {
        output_node_id: nodes[2].id,
        nodes,
        edges,
        node_positions: Default::default(),
        working_space_override: None,
        frames: vec![],
        sticky_notes: vec![],
    }
}

/// The clip graded with `graph`, in a sequence `seq` pixels in size,
/// rendered `w` by `h`.
fn render(graph: &NodeGraph, seq: (u32, u32), w: u32, h: u32, gpu: bool) -> Vec<f32> {
    let mut clip = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    clip.set_grade(graph.clone());
    let mut s = Sequence::new_default();
    s.resolution = Resolution { width: seq.0, height: seq.1 };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip];
    s.tracks = vec![track];
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.render(&s, tc(1.0), w, h, &mut Chart, RenderOptions::default())
}

/// `full` shrunk 2x by averaging each 2x2 block.
fn shrink(full: &[f32]) -> Vec<f32> {
    let (w, h) = (W / 2, H / 2);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            for c in 0..4 {
                let at = |dx: u32, dy: u32| full[(((2 * y + dy) * W + 2 * x + dx) * 4 + c) as usize];
                out.push((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) * 0.25);
            }
        }
    }
    out
}

fn mean_diff(a: &[f32], b: &[f32]) -> f32 {
    let d: f32 = a.chunks_exact(4).zip(b.chunks_exact(4)).map(|(p, q)| (0..3).map(|c| (p[c] - q[c]).abs()).sum::<f32>()).sum();
    d / (a.len() / 4 * 3) as f32
}

fn p(set: impl FnOnce(&mut NodeParams)) -> NodeParams {
    let mut p = NodeParams::default();
    set(&mut p);
    p
}

fn cases() -> Vec<(&'static str, NodeGraph)> {
    let window = QualifierSettings {
        enabled: true,
        hue_center: 30.0,
        hue_width: 60.0,
        hue_soft: 20.0,
        sat_low: 0.0,
        sat_high: 1.0,
        sat_soft: 0.1,
        lum_low: 0.0,
        lum_high: 1.0,
        lum_soft: 0.1,
        ..Default::default()
    };
    vec![
        ("qualifier shrink/grow", graph(NodeKind::Qualifier, p(|p| p.qualifier = Some(QualifierSettings { shrink_grow: 6.0, ..window.clone() })))),
        ("qualifier blur", graph(NodeKind::Qualifier, p(|p| p.qualifier = Some(QualifierSettings { blur: 6.0, ..window.clone() })))),
        ("denoise radius", graph(NodeKind::Denoise, p(|p| {
            p.denoise_luma = Some(0.6);
            p.denoise_chroma = Some(0.6);
            p.denoise_radius = Some(4);
        }))),
        ("grain size", graph(NodeKind::FilmGrain, p(|p| {
            p.grain = Some(FilmGrainSettings { enabled: true, amount: 0.6, size: 2.0, ..Default::default() })
        }))),
        ("clarity", graph(NodeKind::Contrast, p(|p| p.clarity = Some(0.8)))),
    ]
}

#[test]
fn a_half_size_render_grades_like_the_full_one_shrunk() {
    for gpu in [false, true] {
        for (name, g) in cases() {
            let want = shrink(&render(&g, (W, H), W, H, gpu));
            let scaled = render(&g, (W, H), W / 2, H / 2, gpu);
            let unscaled = render(&g, (W / 2, H / 2), W / 2, H / 2, gpu);
            let (new, old) = (mean_diff(&scaled, &want), mean_diff(&unscaled, &want));
            eprintln!("{name} (gpu {gpu}): scaled {new:.5}, in its own pixels {old:.5}");
            assert!(old > 0.002, "{name}: the setting does not depend on the render size here ({old})");
            // What is left is the shrink itself (a 2x2 average is not a
            // half-size render) and whole-pixel rounding.
            assert!(new < old * 0.75 && new < 0.015, "{name} (gpu {gpu}): the half render is not scaled: {new} against {old} unscaled");
        }
    }
}
