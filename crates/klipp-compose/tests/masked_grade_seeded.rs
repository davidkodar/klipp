//! A clip's grade with a window on a Corrector's mask input draws the
//! window when the clip's frame arrives on the GPU, as the viewer and
//! export hand it over, exactly as it does from CPU pixels (#1234). The
//! executor refuses a gate it can't blend from a GPU source, and the
//! renderer runs it again from the picture read back; before, the
//! correction covered the whole frame. A missing GPU leaves it
//! unverified; with `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use std::sync::Arc;

use klipp_compose::{FrameSource, GpuPixels, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::RationalTime;
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

const W: u32 = 64;
const H: u32 = 36;

/// A warm ramp as sRGB bytes and the linear floats they decode to.
fn picture() -> (Arc<Vec<u8>>, Arc<Vec<f32>>) {
    let srgb: Vec<u8> = (0..W * H)
        .flat_map(|i| {
            let v = (40 + (i % W) * 3 + (i / W) * 2) as u8;
            [v, (v as u32 * 4 / 5) as u8, (v as u32 / 2) as u8, 255]
        })
        .collect();
    let linear = srgb.chunks_exact(4).flat_map(|p| {
        let f = |c: u8| klipp_color::transfer::srgb_to_linear(c as f32 / 255.0);
        [f(p[0]), f(p[1]), f(p[2]), 1.0]
    }).collect();
    (Arc::new(srgb), Arc::new(linear))
}

struct Seeded {
    srgb: Arc<Vec<u8>>,
    linear: Arc<Vec<f32>>,
}

impl GpuPixels for Seeded {
    fn seed(&self, exec: &mut klipp_nodes::GraphExecutor, width: u32, height: u32) -> bool {
        exec.prime_chain_from_srgb8(&self.srgb, width, height)
    }
    fn read(&self) -> Arc<Vec<f32>> {
        Arc::clone(&self.linear)
    }
}

/// The clip's frame, on the CPU or on the GPU.
struct Source {
    on_gpu: bool,
}

impl FrameSource for Source {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        assert_eq!((w, h), (W, H));
        let (srgb, linear) = picture();
        let pixels = if self.on_gpu { SourcePixels::Gpu(Arc::new(Seeded { srgb, linear })) } else { SourcePixels::Linear(linear) };
        Some(SourceFrame { pixels, width: w, height: h, pts: None })
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

/// One clip graded by a Corrector (exposure +1) masked by a centred
/// rectangle window: the grade Codex's batch built in #1234.
fn sequence() -> Sequence {
    use klipp_core::{Mask, MaskShape, MaskStack};
    let node = |kind: NodeKind, params: NodeParams| Node {
        id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params,
        keyframes: Default::default(), output_link: None, input_links: vec![],
    };
    let s = node(NodeKind::Source, NodeParams::default());
    let c = node(NodeKind::Corrector, NodeParams { exposure: Some(1.0), ..Default::default() });
    let shape = MaskShape::Rectangle { center_x: 0.5, center_y: 0.5, width: 0.4, height: 0.4, rotation: 0.0 };
    let m = node(NodeKind::Mask, NodeParams { masks: Some(MaskStack { masks: vec![Mask { shape, feather: 0.0, ..Mask::default() }] }), ..Default::default() });
    let o = node(NodeKind::Output, NodeParams::default());
    let link = |a: &Node, b: &Node, port: u8| Edge { from_node: a.id, from_port: 0, to_node: b.id, to_port: port };
    let edges = vec![link(&s, &c, 0), link(&c, &o, 0), link(&m, &c, 1)];
    let graph = NodeGraph { output_node_id: o.id, nodes: vec![s, c, m, o], edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] };
    let mut clip = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    clip.set_grade(graph);
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    seq
}

fn render(on_gpu: bool) -> Vec<f32> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    r.render(&sequence(), tc(1.0), W, H, &mut Source { on_gpu }, RenderOptions::default())
}

fn px(v: &[f32], x: u32, y: u32) -> f32 {
    v[((y * W + x) * 4) as usize]
}

#[test]
fn a_masked_corrector_draws_its_window_from_a_gpu_source() {
    let ok = klipp_nodes::GraphExecutor::new().gpu_pipeline_or_init().is_some();
    assert!(ok || std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
    if !ok {
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let (cpu, gpu) = (render(false), render(true));
    let worst = cpu.iter().zip(&gpu).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    eprintln!("cpu vs gpu source: worst {worst}; inside {} / {}, outside {} / {}", px(&cpu, W / 2, H / 2), px(&gpu, W / 2, H / 2), px(&cpu, 2, 2), px(&gpu, 2, 2));
    assert!(px(&cpu, W / 2, H / 2) > px(&cpu, 2, 2) * 2.0, "the CPU source draws the window");
    assert!(worst < 3e-3, "the GPU source draws what the CPU source draws (worst {worst})");
}
